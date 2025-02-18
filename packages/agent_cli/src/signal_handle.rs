use std::sync::{Arc, LazyLock};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tokio::signal::ctrl_c;

static SIGNAL: LazyLock<SignalHandle> = LazyLock::new(SignalHandle::setup);

pub fn get_signal_handle() -> SignalHandle {
    SIGNAL.clone()
}

#[derive(Clone)]
pub struct SignalHandle {
    inner: Arc<Inner>,
}

pub struct SignalRequireCloseGuard {
    inner: Arc<Inner>,
}

impl Drop for SignalRequireCloseGuard {
    fn drop(&mut self) {
        self.inner.confirm_close.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Inner {
    confirm_close: AtomicU32,
    close_requested: AtomicBool,
}

impl SignalHandle {
    pub fn is_confirming_close(&self) -> bool {
        self.inner.close_requested.load(Ordering::SeqCst)
    }

    pub fn decline_close(&self) {
        self.inner.close_requested.store(false, Ordering::SeqCst);
    }

    pub fn close_guard(&self) -> SignalRequireCloseGuard {
        let inner = self.inner.clone();
        inner.confirm_close.fetch_add(1, Ordering::SeqCst);

        SignalRequireCloseGuard {
            inner
        }
    }
}

impl SignalHandle {
    fn setup() -> Self {
        let signal = SignalHandle {
            inner: Arc::new(Inner {
                confirm_close: AtomicU32::new(0),
                close_requested: AtomicBool::new(false)
            })
        };

        let inner = signal.inner.clone();
        tokio::spawn(async move {
            loop {
                if let Err(error) = ctrl_c().await {
                    tracing::error!(?error, "cannot listen for ctrl_c");
                    break;
                }

                tracing::info!("received Ctrl+C signal");

                if inner.confirm_close.load(Ordering::SeqCst) == 0 {
                    tracing::info!("no Ctrl+C handler set, closing program");
                    std::process::exit(0);
                }

                if inner.close_requested.swap(true, Ordering::SeqCst) {
                    tracing::info!("Close requested twice, exiting program");
                    std::process::exit(0);
                }
            }
        });

        signal
    }
}
