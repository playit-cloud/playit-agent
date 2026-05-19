use std::num::NonZeroU32;
use std::sync::Arc;

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_ipc::model::{LogEntry, LogLevel, ServiceUpdate};
use tokio::sync::broadcast;
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::dynamic_filter_fn;
use tracing_subscriber::layer::Context;
use tracing_subscriber::layer::Filter;

pub const LOG_RATE_LIMIT_PER_SECOND: u32 = 2;
pub const LOG_RATE_LIMIT_BURST: u32 = 32;

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
            return;
        }

        if !self.message.is_empty() {
            self.message.push_str(", ");
        }
        self.message
            .push_str(&format!("{}={value:?}", field.name()));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
            return;
        }

        if !self.message.is_empty() {
            self.message.push_str(", ");
        }
        self.message.push_str(&format!("{}={value}", field.name()));
    }
}

fn level_to_wire(level: &tracing::Level) -> LogLevel {
    match *level {
        tracing::Level::TRACE => LogLevel::Trace,
        tracing::Level::DEBUG => LogLevel::Debug,
        tracing::Level::INFO => LogLevel::Info,
        tracing::Level::WARN => LogLevel::Warn,
        tracing::Level::ERROR => LogLevel::Error,
    }
}

#[derive(Clone)]
struct LogRateLimiter {
    limiter: Arc<DefaultDirectRateLimiter>,
}

impl LogRateLimiter {
    fn new() -> Self {
        let rate = NonZeroU32::new(LOG_RATE_LIMIT_PER_SECOND)
            .expect("log rate limit per second must be non-zero");
        let burst =
            NonZeroU32::new(LOG_RATE_LIMIT_BURST).expect("log rate limit burst must be non-zero");

        Self {
            limiter: Arc::new(RateLimiter::direct(
                Quota::per_second(rate).allow_burst(burst),
            )),
        }
    }

    fn allow(&self, metadata: &Metadata<'_>) -> bool {
        !metadata.is_event() || self.allow_event()
    }

    fn allow_event(&self) -> bool {
        self.limiter.check().is_ok()
    }
}

pub fn log_rate_limit_filter<S>() -> impl Filter<S>
where
    S: Subscriber,
{
    let limiter = LogRateLimiter::new();
    dynamic_filter_fn(move |metadata, _ctx| limiter.allow(metadata))
}

/// Tracing layer that broadcasts log events via IPC.
pub struct IpcBroadcastLayer {
    event_tx: broadcast::Sender<ServiceUpdate>,
}

impl IpcBroadcastLayer {
    pub fn new(event_tx: broadcast::Sender<ServiceUpdate>) -> Self {
        Self { event_tx }
    }
}

impl<S: Subscriber> Layer<S> for IpcBroadcastLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        use playit_agent_core::utils::now_milli;

        let metadata = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let _ = self.event_tx.send(ServiceUpdate::Log(LogEntry {
            level: level_to_wire(metadata.level()),
            target: metadata.target().to_string(),
            message: visitor.message,
            timestamp: now_milli(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{LOG_RATE_LIMIT_BURST, LogRateLimiter};

    #[test]
    fn log_rate_limiter_allows_initial_burst() {
        let limiter = LogRateLimiter::new();

        for _ in 0..LOG_RATE_LIMIT_BURST {
            assert!(limiter.allow_event());
        }
    }

    #[test]
    fn log_rate_limiter_rejects_after_burst() {
        let limiter = LogRateLimiter::new();

        for _ in 0..LOG_RATE_LIMIT_BURST {
            assert!(limiter.allow_event());
        }

        assert!(!limiter.allow_event());
    }

    #[test]
    fn log_rate_limiter_refills_at_sustained_rate() {
        let limiter = LogRateLimiter::new();

        for _ in 0..LOG_RATE_LIMIT_BURST {
            assert!(limiter.allow_event());
        }
        assert!(!limiter.allow_event());

        std::thread::sleep(Duration::from_millis(1_100));

        let accepted = (0..4).filter(|_| limiter.allow_event()).count();
        assert!((2..=3).contains(&accepted));
    }
}
