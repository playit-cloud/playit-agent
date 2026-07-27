use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::{fmt, io};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::stats::AgentStats;
use crate::utils::now_milli;

const TCP_PIPE_BUFFER_SIZE: usize = 16 * 1024;

/// Direction of data flow for stats tracking
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeDirection {
    /// Data flowing from tunnel to local origin (bytes in)
    TunnelToOrigin,
    /// Data flowing from local origin to tunnel (bytes out)
    OriginToTunnel,
}

impl fmt::Display for PipeDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TunnelToOrigin => f.write_str("connecting client -> local server"),
            Self::OriginToTunnel => f.write_str("local server -> connecting client"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PipeCloseKind {
    Eof,
    ReadError,
    WriteError,
    Cancelled,
}

#[derive(Clone, Debug)]
pub(crate) struct PipeClose {
    pub(crate) direction: PipeDirection,
    pub(crate) kind: PipeCloseKind,
    pub(crate) error_kind: Option<io::ErrorKind>,
    pub(crate) error: Option<String>,
    pub(crate) finished_at: u64,
}

pub struct TcpPipe {
    cancel: CancellationToken,
    shared: Arc<Shared>,
}

struct Shared {
    last_activity: AtomicU64,
    bytes_written: AtomicU64,
    close: Mutex<Option<PipeClose>>,
    closed: Notify,
}

#[derive(Clone)]
pub(crate) struct TcpPipeCloseHandle {
    shared: Arc<Shared>,
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
            close: Mutex::new(None),
            closed: Notify::new(),
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
            .start()
            .instrument(tracing::Span::current()),
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

    pub(crate) fn close_handle(&self) -> TcpPipeCloseHandle {
        TcpPipeCloseHandle {
            shared: self.shared.clone(),
        }
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
        let mut buffer = vec![0u8; TCP_PIPE_BUFFER_SIZE];

        let close = loop {
            // Keep the pipe cooperative when both sockets stay continuously ready.
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    break PipeClose {
                        direction: self.direction,
                        kind: PipeCloseKind::Cancelled,
                        error_kind: None,
                        error: None,
                        finished_at: now_milli(),
                    };
                }
                _ = tokio::task::yield_now() => {}
            }

            let Some(read_res) = self
                .cancel
                .run_until_cancelled(self.from.read(&mut buffer[..]))
                .await
            else {
                break PipeClose {
                    direction: self.direction,
                    kind: PipeCloseKind::Cancelled,
                    error_kind: None,
                    error: None,
                    finished_at: now_milli(),
                };
            };

            let byte_count = match read_res {
                Ok(count) => count,
                Err(error) => {
                    log_pipe_error(self.direction, PipeCloseKind::ReadError, &error);
                    self.cancel.cancel();
                    break PipeClose {
                        direction: self.direction,
                        kind: PipeCloseKind::ReadError,
                        error_kind: Some(error.kind()),
                        error: Some(error.to_string()),
                        finished_at: now_milli(),
                    };
                }
            };

            if byte_count == 0 {
                if let Err(error) = self.to.shutdown().await {
                    tracing::debug!(
                        direction = %self.direction,
                        error = %error,
                        "Failed to propagate TCP half-close"
                    );
                }
                tracing::debug!(direction = %self.direction, "TCP pipe reached EOF");
                break PipeClose {
                    direction: self.direction,
                    kind: PipeCloseKind::Eof,
                    error_kind: None,
                    error: None,
                    finished_at: now_milli(),
                };
            }

            let Some(write_res) = self
                .cancel
                .run_until_cancelled(self.to.write_all(&buffer[..byte_count]))
                .await
            else {
                break PipeClose {
                    direction: self.direction,
                    kind: PipeCloseKind::Cancelled,
                    error_kind: None,
                    error: None,
                    finished_at: now_milli(),
                };
            };

            if let Err(error) = write_res {
                log_pipe_error(self.direction, PipeCloseKind::WriteError, &error);
                self.cancel.cancel();
                break PipeClose {
                    direction: self.direction,
                    kind: PipeCloseKind::WriteError,
                    error_kind: Some(error.kind()),
                    error: Some(error.to_string()),
                    finished_at: now_milli(),
                };
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
        };

        self.shared.last_activity.store(u64::MAX, Ordering::Release);
        *self
            .shared
            .close
            .lock()
            .expect("TCP pipe close lock poisoned") = Some(close);
        self.shared.closed.notify_waiters();
    }
}

impl TcpPipeCloseHandle {
    pub(crate) async fn wait(&self) -> PipeClose {
        loop {
            let notified = self.shared.closed.notified();
            if let Some(close) = self
                .shared
                .close
                .lock()
                .expect("TCP pipe close lock poisoned")
                .clone()
            {
                return close;
            }
            notified.await;
        }
    }
}

fn log_pipe_error(direction: PipeDirection, close_kind: PipeCloseKind, error: &io::Error) {
    let expected = matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::NotConnected
    );

    if expected {
        tracing::debug!(
            direction = %direction,
            operation = ?close_kind,
            error = %error,
            "TCP pipe closed"
        );
    } else {
        tracing::warn!(
            direction = %direction,
            operation = ?close_kind,
            error = %error,
            "TCP connection ended because of an unexpected I/O failure"
        );
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::sync::CancellationToken;

    use super::{PipeCloseKind, PipeDirection, TcpPipe};

    #[tokio::test]
    async fn eof_propagates_half_close() {
        let (mut input, input_reader) = tokio::io::duplex(64);
        let (mut output_reader, output) = tokio::io::duplex(64);
        let pipe = TcpPipe::new_with_stats(
            CancellationToken::new(),
            input_reader,
            output,
            None,
            PipeDirection::TunnelToOrigin,
        );
        let close = pipe.close_handle();

        input.write_all(b"hello").await.unwrap();
        input.shutdown().await.unwrap();

        let mut received = Vec::new();
        output_reader.read_to_end(&mut received).await.unwrap();
        let outcome = close.wait().await;

        assert_eq!(received, b"hello");
        assert_eq!(outcome.kind, PipeCloseKind::Eof);
        assert_eq!(outcome.direction, PipeDirection::TunnelToOrigin);
    }
}
