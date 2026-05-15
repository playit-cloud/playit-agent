use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::stats::AgentStats;
use crate::utils::now_milli;

use super::tcp_upload_qos::{TCP_UPLOAD_QOS_SLICE_SIZE, TcpUploadFlow};

const TCP_PIPE_BUFFER_SIZE: usize = 16 * 1024;

/// Direction of data flow for stats tracking
#[derive(Clone, Copy)]
pub enum PipeDirection {
    /// Data flowing from tunnel to local origin (bytes in)
    TunnelToOrigin,
    /// Data flowing from local origin to tunnel (bytes out)
    OriginToTunnel,
}

pub struct TcpPipe {
    cancel: CancellationToken,
    shared: Arc<Shared>,
}

struct Shared {
    last_activity: AtomicU64,
    bytes_written: AtomicU64,
}

impl TcpPipe {
    pub fn new<R: AsyncRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static>(
        from: R,
        to: W,
    ) -> Self {
        Self::new_with_cancel(Default::default(), from, to)
    }

    pub fn new_with_cancel<
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    >(
        cancel: CancellationToken,
        from: R,
        to: W,
    ) -> Self {
        Self::new_with_stats(cancel, from, to, None, PipeDirection::TunnelToOrigin)
    }

    pub fn new_with_stats<
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    >(
        cancel: CancellationToken,
        from: R,
        to: W,
        stats: Option<AgentStats>,
        direction: PipeDirection,
    ) -> Self {
        Self::new_with_stats_and_upload_flow(cancel, from, to, stats, direction, None)
    }

    pub(super) fn new_with_stats_and_upload_flow<
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    >(
        cancel: CancellationToken,
        from: R,
        to: W,
        stats: Option<AgentStats>,
        direction: PipeDirection,
        upload_flow: Option<TcpUploadFlow>,
    ) -> Self {
        let shared = Arc::new(Shared {
            last_activity: AtomicU64::new(now_milli()),
            bytes_written: AtomicU64::new(0),
        });

        let this = TcpPipe { cancel, shared };

        tokio::spawn(
            Worker {
                cancel: this.cancel.clone(),
                shared: this.shared.clone(),
                from,
                to,
                stats,
                direction,
                upload_flow,
            }
            .start(),
        );

        this
    }

    pub fn bytes_written(&self) -> u64 {
        self.shared.bytes_written.load(Ordering::Acquire)
    }

    pub fn last_activity(&self) -> u64 {
        let value = self.shared.last_activity.load(Ordering::Acquire);

        if value == u64::MAX { 0 } else { value }
    }

    pub fn is_closed(&self) -> bool {
        self.shared.last_activity.load(Ordering::Acquire) == u64::MAX
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

impl Drop for TcpPipe {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Worker<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> {
    cancel: CancellationToken,
    shared: Arc<Shared>,
    from: R,
    to: W,
    stats: Option<AgentStats>,
    direction: PipeDirection,
    upload_flow: Option<TcpUploadFlow>,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Worker<R, W> {
    pub async fn start(mut self) {
        let buffer_size = if self.upload_flow.is_some() {
            TCP_UPLOAD_QOS_SLICE_SIZE
        } else {
            TCP_PIPE_BUFFER_SIZE
        };
        let mut buffer = vec![0u8; buffer_size];

        loop {
            // Keep the pipe cooperative when both sockets stay continuously ready.
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    tracing::info!("TcpPipe cancelled");
                    break;
                }
                _ = tokio::task::yield_now() => {}
            }

            let Some(read_res) = self
                .cancel
                .run_until_cancelled(self.from.read(&mut buffer[..]))
                .await
            else {
                tracing::info!("TcpPipe cancelled");
                break;
            };

            let byte_count = match read_res {
                Ok(count) => count,
                Err(error) => {
                    tracing::error!(?error, "failed to read data");
                    break;
                }
            };

            if byte_count == 0 {
                tracing::info!("pipe ended due to EOF");
                break;
            }

            if let Some(upload_flow) = &self.upload_flow
                && !upload_flow.acquire(byte_count, &self.cancel).await
            {
                tracing::info!("TcpPipe upload QoS acquire failed");
                break;
            }

            let Some(write_res) = self
                .cancel
                .run_until_cancelled(self.to.write_all(&buffer[..byte_count]))
                .await
            else {
                tracing::info!("TcpPipe cancelled");
                break;
            };

            if let Err(error) = write_res {
                tracing::error!(?error, "failed to write data");
                break;
            }

            self.shared
                .last_activity
                .store(now_milli(), Ordering::Release);
            self.shared
                .bytes_written
                .fetch_add(byte_count as u64, Ordering::AcqRel);

            // Update global stats if provided
            if let Some(ref stats) = self.stats {
                let bytes = byte_count as u64;
                match self.direction {
                    PipeDirection::TunnelToOrigin => stats.add_bytes_in(bytes),
                    PipeDirection::OriginToTunnel => stats.add_bytes_out(bytes),
                }
            }
        }

        self.shared.last_activity.store(u64::MAX, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        time::{Duration, timeout},
    };

    use super::*;
    use crate::network::tcp::tcp_upload_qos::{TCP_UPLOAD_QOS_SLICE_SIZE, TcpUploadFairness};

    async fn wait_for_bytes(pipe: &TcpPipe, expected: u64) {
        timeout(Duration::from_secs(1), async {
            while pipe.bytes_written() != expected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("pipe did not reach expected byte count");
    }

    #[tokio::test]
    async fn upload_qos_pipe_reads_in_qos_sized_slices() {
        let cancel = CancellationToken::new();
        let fairness = TcpUploadFairness::new(cancel.child_token());
        let upload_flow = fairness.register();
        let (mut source_write, source_read) = tokio::io::duplex(TCP_UPLOAD_QOS_SLICE_SIZE * 4);
        let (sink_write, mut sink_read) = tokio::io::duplex(TCP_UPLOAD_QOS_SLICE_SIZE);

        let pipe = TcpPipe::new_with_stats_and_upload_flow(
            cancel.clone(),
            source_read,
            sink_write,
            None,
            PipeDirection::OriginToTunnel,
            Some(upload_flow),
        );

        let payload = vec![7u8; TCP_UPLOAD_QOS_SLICE_SIZE * 2];
        let write_task = tokio::spawn(async move {
            source_write
                .write_all(&payload)
                .await
                .expect("source write should succeed");
        });

        wait_for_bytes(&pipe, TCP_UPLOAD_QOS_SLICE_SIZE as u64).await;

        let mut read_buffer = vec![0u8; TCP_UPLOAD_QOS_SLICE_SIZE];
        sink_read
            .read_exact(&mut read_buffer)
            .await
            .expect("sink read should succeed");

        wait_for_bytes(&pipe, (TCP_UPLOAD_QOS_SLICE_SIZE * 2) as u64).await;
        write_task.await.expect("source writer should finish");

        cancel.cancel();
    }
}
