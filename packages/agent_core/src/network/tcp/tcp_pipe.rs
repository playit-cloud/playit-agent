use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
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
    task: Option<tokio::task::JoinHandle<crate::playit_agent::ServiceExit>>,
}

struct Shared {
    last_activity: AtomicU64,
    bytes_written: AtomicU64,
    exit: Mutex<Option<crate::playit_agent::ServiceExit>>,
}

impl TcpPipe {
    pub fn spawn_with_cancel<
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    >(
        cancel: CancellationToken,
        from: R,
        to: W,
    ) -> Self {
        Self::spawn_with_stats(cancel, from, to, None, PipeDirection::TunnelToOrigin)
    }

    pub fn spawn_with_stats<
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
            exit: Mutex::new(None),
        });

        let task_cancel = cancel.clone();
        let task_shared = shared.clone();
        let task = tokio::spawn(
            Worker {
                cancel: task_cancel,
                shared: task_shared.clone(),
                from,
                to,
                stats,
                direction,
            }
            .start_and_record(),
        );

        TcpPipe {
            cancel,
            shared,
            task: Some(task),
        }
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

    pub fn exit(&self) -> Option<crate::playit_agent::ServiceExit> {
        let recorded = self.shared.exit.lock().ok().and_then(|exit| exit.clone());
        if recorded.is_some() {
            return recorded;
        }
        self.task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
            .then(|| {
                crate::playit_agent::ServiceExit::Panicked(
                    "TCP pipe ended without recording an exit".to_owned(),
                )
            })
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
    }

    pub async fn join(mut self, deadline: std::time::Duration) -> crate::playit_agent::ServiceExit {
        self.cancel.cancel();
        let Some(mut task) = self.task.take() else {
            return crate::playit_agent::ServiceExit::Completed;
        };
        match tokio::time::timeout(deadline, &mut task).await {
            Ok(Ok(exit)) => exit,
            Ok(Err(error)) => crate::playit_agent::ServiceExit::Panicked(error.to_string()),
            Err(_) => {
                task.abort();
                let _ = task.await;
                crate::playit_agent::ServiceExit::DeadlineExceeded
            }
        }
    }
}

impl Drop for TcpPipe {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
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
    async fn start_and_record(self) -> crate::playit_agent::ServiceExit {
        let shared = self.shared.clone();
        let exit = self.start().await;
        if let Ok(mut slot) = shared.exit.lock() {
            *slot = Some(exit.clone());
        }
        exit
    }

    pub async fn start(mut self) -> crate::playit_agent::ServiceExit {
        let mut buffer = vec![0u8; TCP_PIPE_BUFFER_SIZE];

        loop {
            // Keep the pipe cooperative when both sockets stay continuously ready.
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    tracing::debug!("TcpPipe cancelled");
                    self.shared.last_activity.store(u64::MAX, Ordering::Release);
                    return crate::playit_agent::ServiceExit::Cancelled;
                }
                _ = tokio::task::yield_now() => {}
            }

            let Some(read_res) = self
                .cancel
                .run_until_cancelled(self.from.read(&mut buffer[..]))
                .await
            else {
                tracing::debug!("TcpPipe cancelled");
                self.shared.last_activity.store(u64::MAX, Ordering::Release);
                return crate::playit_agent::ServiceExit::Cancelled;
            };

            let byte_count = match read_res {
                Ok(count) => count,
                Err(error) => {
                    tracing::error!(?error, "failed to read data");
                    self.shared.last_activity.store(u64::MAX, Ordering::Release);
                    return crate::playit_agent::ServiceExit::Io {
                        operation: "read TCP stream".to_owned(),
                        message: error.to_string(),
                    };
                }
            };

            if byte_count == 0 {
                tracing::debug!("pipe ended due to EOF");
                self.shared.last_activity.store(u64::MAX, Ordering::Release);
                return crate::playit_agent::ServiceExit::CleanEof;
            }

            let write = tokio::select! {
                _ = self.cancel.cancelled() => {
                    self.shared.last_activity.store(u64::MAX, Ordering::Release);
                    return crate::playit_agent::ServiceExit::Cancelled;
                }
                write = self.to.write_all(&buffer[..byte_count]) => write,
            };
            if let Err(error) = write {
                tracing::error!(?error, "failed to write data");
                self.shared.last_activity.store(u64::MAX, Ordering::Release);
                return crate::playit_agent::ServiceExit::Io {
                    operation: "write TCP stream".to_owned(),
                    message: error.to_string(),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playit_agent::ServiceExit;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;
    use tokio::io::{AsyncWrite, ReadBuf, sink};

    struct ErrorReader;

    impl AsyncRead for ErrorReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "fixture",
            )))
        }
    }

    async fn wait_for_exit(pipe: &TcpPipe) -> ServiceExit {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(exit) = pipe.exit() {
                    return exit;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("TCP pipe reported an exit")
    }

    #[tokio::test]
    async fn clean_eof_has_a_typed_exit() {
        let pipe = TcpPipe::spawn_with_cancel(CancellationToken::new(), tokio::io::empty(), sink());
        assert_eq!(wait_for_exit(&pipe).await, ServiceExit::CleanEof);
    }

    #[tokio::test]
    async fn read_failure_has_a_typed_exit() {
        let pipe = TcpPipe::spawn_with_cancel(CancellationToken::new(), ErrorReader, sink());
        assert!(matches!(
            wait_for_exit(&pipe).await,
            ServiceExit::Io { operation, .. } if operation == "read TCP stream"
        ));
    }

    #[tokio::test]
    async fn cancellation_stops_a_pending_pipe() {
        let (reader, _peer) = tokio::io::duplex(16);
        let pipe = TcpPipe::spawn_with_cancel(CancellationToken::new(), reader, PendingWriter);
        pipe.shutdown();
        assert_eq!(wait_for_exit(&pipe).await, ServiceExit::Cancelled);
    }

    struct PendingWriter;

    impl AsyncWrite for PendingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
}
