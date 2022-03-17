use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::task::JoinHandle;

#[derive(Clone)]
pub struct DependentTask<T> {
    inner: Arc<Inner<T>>,
}

impl<T> DependentTask<T> {
    pub fn new(task: JoinHandle<T>) -> Self {
        DependentTask {
            inner: Arc::new(Inner {
                task,
                count: AtomicUsize::new(1),
            }),
        }
    }
}

impl<T> Clone for DependentTask<T> {
    fn clone(&self) -> Self {
        self.inner.count.fetch_add(1, Ordering::SeqCst);
        DependentTask {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for DependentTask<T> {
    fn drop(&mut self) {
        let prev_value = self.inner.count.fetch_sub(1, Ordering::SeqCst);
        if prev_value == 1 {
            self.inner.task.abort();
        }
    }
}

struct Inner<T> {
    task: JoinHandle<T>,
    count: AtomicUsize,
}
