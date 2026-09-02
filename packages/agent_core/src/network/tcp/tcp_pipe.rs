use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::stats::AgentStats;
use crate::utils::now_milli;

const TCP_PIPE_BUFFER_SIZE: usize = 16 * 1024;

/// Direction of data flow for stats tracking
#[derive(Clone, Copy, Debug)]
pub enum PipeDirection {
    /// Data flowing from tunnel to local origin (bytes in)
    TunnelToOrigin,
    /// Data flowing from local origin to tunnel (bytes out)
    OriginToTunnel,
}

impl PipeDirection {
    /// Name of the peer this pipe reads from.
    fn read_source(self) -> &'static str {
        match self {
            Self::TunnelToOrigin => "tunnel",
            Self::OriginToTunnel => "origin",
        }
    }

    /// Name of the peer this pipe writes to.
    fn write_destination(self) -> &'static str {
        match self {
            Self::TunnelToOrigin => "origin",
            Self::OriginToTunnel => "tunnel",
        }
    }
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
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Worker<R, W> {
    pub async fn start(mut self) {
        let direction = self.direction;
        let read_source = direction.read_source();
        let write_destination = direction.write_destination();
        let mut buffer = vec![0u8; TCP_PIPE_BUFFER_SIZE];

        loop {
            // Keep the pipe cooperative when both sockets stay continuously ready.
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    tracing::debug!(?direction, "pipe cancelled before next read");
                    break;
                }
                _ = tokio::task::yield_now() => {}
            }

            let Some(read_res) = self
                .cancel
                .run_until_cancelled(self.from.read(&mut buffer[..]))
                .await
            else {
                tracing::debug!(
                    ?direction,
                    "pipe cancelled while awaiting data from {read_source}"
                );
                break;
            };

            let byte_count = match read_res {
                Ok(count) => count,
                Err(error) => {
                    tracing::debug!(
                        ?direction,
                        ?error,
                        "read from {read_source} failed; closing pipe"
                    );
                    break;
                }
            };

            if byte_count == 0 {
                tracing::debug!(
                    ?direction,
                    "{read_source} closed the connection (EOF); closing pipe"
                );
                break;
            }

            if let Err(error) = self.to.write_all(&buffer[..byte_count]).await {
                tracing::debug!(
                    ?direction,
                    bytes = byte_count,
                    ?error,
                    "write to {write_destination} failed; closing pipe"
                );
                break;
            }

            tracing::trace!(
                ?direction,
                bytes = byte_count,
                "forwarded chunk from {read_source} to {write_destination}"
            );

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
