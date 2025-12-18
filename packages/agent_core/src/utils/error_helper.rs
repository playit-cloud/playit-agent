use std::{
    fmt::Debug,
    time::{Duration, Instant},
};

pub trait ErrorHelper<T: Debug> {
    fn with_error<F: FnOnce(&T)>(self, f: F) -> Self;
    fn take_error<F: FnOnce(T)>(self, f: F);
}

impl<R, E: Debug> ErrorHelper<E> for Result<R, E> {
    fn with_error<F: FnOnce(&E)>(self, f: F) -> Self {
        match self {
            Ok(ok) => Ok(ok),
            Err(error) => {
                f(&error);
                Err(error)
            }
        }
    }

    fn take_error<F: FnOnce(E)>(self, f: F) {
        if let Err(error) = self {
            f(error);
        }
    }
}

pub struct MaxErrorInterval {
    last_report: Instant,
    min_wait: Duration,
    redundant_calls: usize,
}

impl MaxErrorInterval {
    pub fn new(interval: Duration) -> Self {
        MaxErrorInterval {
            last_report: Instant::now() - interval * 2,
            min_wait: interval,
            redundant_calls: 0,
        }
    }

    pub fn check(&mut self) -> bool {
        let now = Instant::now();

        if self.min_wait <= now - self.last_report {
            self.last_report = now;
            true
        } else {
            false
        }
    }

    pub fn calls(&mut self) -> usize {
        std::mem::replace(&mut self.redundant_calls, 0)
    }
}
