use std::sync::{atomic::{AtomicU64, Ordering}, Arc};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::utils::now_milli;

pub struct TcpPipe {
    cancel: CancellationToken,
    shared: Arc<Shared>,
}

struct Shared {
    last_activity: AtomicU64,
    bytes_written: AtomicU64,
}

impl TcpPipe {
    pub fn new<R: AsyncRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static>(from: R, to: W) -> Self {
        Self::new_with_cancel(Default::default(), from, to)
    }

    pub fn new_with_cancel<R: AsyncRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static>(cancel: CancellationToken, from: R, to: W) -> Self {
        let shared = Arc::new(Shared {
            last_activity: AtomicU64::new(now_milli()),
            bytes_written: AtomicU64::new(0),
        });

        let this = TcpPipe {
            cancel,
            shared,
        };

        tokio::spawn(Worker {
            cancel: this.cancel.clone(),
            shared: this.shared.clone(),
            from,
            to,
        }.start());

        this
    }

    pub fn bytes_written(&self) -> u64 {
        self.shared.bytes_written.load(Ordering::Acquire)
    }

    pub fn last_activity(&self) -> u64 {
        let value = self.shared.last_activity.load(Ordering::Acquire);

        if value == u64::MAX {
            0
        } else {
            value
        }
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
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Worker<R, W> {
    pub async fn start(mut self) {
        let mut buffer = vec![0u8; 2048];

        loop {
            tokio::task::yield_now().await;

            let Some(read_res) = self.cancel.run_until_cancelled(self.from.read(&mut buffer[..])).await else {
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

            if let Err(error) = self.to.write_all(&buffer[..byte_count]).await {
                tracing::error!(?error, "failed to write data");
                break;
            }

            self.shared.last_activity.store(now_milli(), Ordering::Release);
            self.shared.bytes_written.fetch_add(byte_count as u64, Ordering::AcqRel);
        }

        self.shared.last_activity.store(u64::MAX, Ordering::Release);
    }
}

