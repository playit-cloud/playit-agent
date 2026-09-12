use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::stats::AgentStats;
use crate::utils::now_milli;

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
    task: tokio::task::JoinHandle<()>,
}

struct Shared {
    closed: AtomicBool,
    activity: std::sync::Mutex<tokio::time::Instant>,
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
        let shared = Arc::new(Shared {
            closed: AtomicBool::new(false),
            activity: std::sync::Mutex::new(tokio::time::Instant::now()),
            last_activity: AtomicU64::new(now_milli()),
            bytes_written: AtomicU64::new(0),
        });

        let worker = Worker {
            cancel: cancel.clone(),
            shared: shared.clone(),
            from,
            to,
            stats,
            direction,
        };
        let task = tokio::spawn(worker.start());
        Self {
            cancel,
            shared,
            task,
        }
    }

    pub fn bytes_written(&self) -> u64 {
        self.shared.bytes_written.load(Ordering::Acquire)
    }

    pub fn last_activity(&self) -> u64 {
        self.shared.last_activity.load(Ordering::Acquire)
    }

    pub fn idle_for(&self) -> std::time::Duration {
        self.shared.activity.lock().unwrap().elapsed()
    }

    pub fn is_closed(&self) -> bool {
        self.task.is_finished() || self.shared.closed.load(Ordering::Acquire)
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

impl Drop for TcpPipe {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

struct Worker<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> {
    cancel: CancellationToken,
    shared: Arc<Shared>,
    from: R,
    to: W,
    stats: Option<AgentStats>,
    direction: PipeDirection,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Worker<R, W> {
    pub async fn start(mut self) {
        let cancel = self.cancel.clone();
        if let Some(Err(error)) = cancel.run_until_cancelled(self.copy()).await {
            tracing::debug!(?error, "TCP forwarding failed");
            cancel.cancel();
        }
        self.shared.closed.store(true, Ordering::Release);
    }

    async fn copy(&mut self) -> std::io::Result<()> {
        let mut buffer = vec![0; TCP_PIPE_BUFFER_SIZE];
        loop {
            let count = self.from.read(&mut buffer).await?;
            if count == 0 {
                // Preserve the reverse direction after a clean half-close.
                return self.to.shutdown().await;
            }
            let mut written = 0;
            while written < count {
                let count = self.to.write(&buffer[written..count]).await?;
                if count == 0 {
                    return Err(std::io::ErrorKind::WriteZero.into());
                }
                written += count;
                self.shared
                    .last_activity
                    .store(now_milli(), Ordering::Release);
                *self.shared.activity.lock().unwrap() = tokio::time::Instant::now();
                self.shared
                    .bytes_written
                    .fetch_add(count as u64, Ordering::AcqRel);
                if let Some(stats) = &self.stats {
                    match self.direction {
                        PipeDirection::TunnelToOrigin => stats.add_bytes_in(count as u64),
                        PipeDirection::OriginToTunnel => stats.add_bytes_out(count as u64),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn shutdown_interrupts_a_blocked_write() {
        let (mut input, reader) = tokio::io::duplex(64);
        let (writer, _unread_output) = tokio::io::duplex(1);
        let pipe = TcpPipe::new(reader, writer);
        input.write_all(b"blocked output").await.unwrap();
        tokio::task::yield_now().await;
        pipe.shutdown();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !pipe.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn eof_shuts_down_destination_without_cancelling_reverse_pipe() {
        let (mut peer, tunnel) = tokio::io::duplex(128);
        let (origin, mut server) = tokio::io::duplex(128);
        let (tunnel_read, tunnel_write) = tokio::io::split(tunnel);
        let (origin_read, origin_write) = tokio::io::split(origin);
        let cancel = CancellationToken::new();
        let incoming = TcpPipe::new_with_cancel(cancel.clone(), tunnel_read, origin_write);
        let outgoing = TcpPipe::new_with_cancel(cancel.clone(), origin_read, tunnel_write);
        peer.write_all(b"request").await.unwrap();
        peer.shutdown().await.unwrap();
        let mut request = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), server.read_to_end(&mut request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request, b"request");
        assert!(!cancel.is_cancelled());
        server.write_all(b"response").await.unwrap();
        server.shutdown().await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), peer.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response, b"response");
        assert_eq!(incoming.bytes_written(), 7);
        assert_eq!(outgoing.bytes_written(), 8);
    }
}
