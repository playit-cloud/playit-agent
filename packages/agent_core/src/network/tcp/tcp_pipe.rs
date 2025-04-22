use std::{sync::{atomic::{AtomicU64, Ordering}, Arc}, u64};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::utils::now_milli;

pub struct TcpPipe {
    cancel: CancellationToken,
    last_activity: Arc<AtomicU64>,
}

impl TcpPipe {
    pub fn new<R: AsyncRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static>(from: R, to: W) -> Self {
        Self::new_with_cancel(Default::default(), from, to)
    }

    pub fn new_with_cancel<R: AsyncRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static>(cancel: CancellationToken, from: R, to: W) -> Self {
        let activity = Arc::new(AtomicU64::new(0));

        let this = TcpPipe {
            cancel,
            last_activity: activity,
        };

        tokio::spawn(Task {
            cancel: this.cancel.clone(),
            last_activity: this.last_activity.clone(),
            from,
            to,
        }.start());

        this
    }

    pub fn last_activity(&self) -> u64 {
        let value = self.last_activity.load(Ordering::Acquire);

        if value == u64::MAX {
            0
        } else {
            value
        }
    }

    pub fn is_closed(&self) -> bool {
        self.last_activity.load(Ordering::Acquire) == u64::MAX
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

struct Task<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> {
    cancel: CancellationToken,
    last_activity: Arc<AtomicU64>,
    from: R,
    to: W,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Task<R, W> {
    pub async fn start(mut self) {
        let mut buffer = vec![0u8; 2048];

        loop {
            tokio::task::yield_now().await;

            let Some(read_res) = self.cancel.run_until_cancelled(self.from.read(&mut buffer[..])).await else {
                tracing::info!("TcpPipe cancelled");
                break;
            };

            let count = match read_res {
                Ok(count) => count,
                Err(error) => {
                    tracing::error!(?error, "failed to read data");
                    break;
                }
            };

            if count == 0 {
                tracing::info!("pipe ended due to EOF");
                break;
            }

            if let Err(error) = self.to.write_all(&buffer[..count]).await {
                tracing::error!(?error, "failed to write data");
                break;
            }

            self.last_activity.store(now_milli(), Ordering::Release);
        }

        self.last_activity.store(u64::MAX, Ordering::Release);
    }
}

