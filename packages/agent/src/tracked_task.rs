use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::task::JoinHandle;

pub struct TrackedTask {
    done: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl TrackedTask {
    pub fn new<T: std::future::Future + Send + 'static>(future: T) -> Self {
        let done = Arc::new(AtomicBool::new(false));

        let report = done.clone();
        let task = tokio::spawn(async move {
            future.await;
            report.store(true, Ordering::SeqCst);
        });

        TrackedTask { done, task }
    }

    pub async fn wait(self) -> anyhow::Result<()> {
        Ok(self.task.await?)
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
}
