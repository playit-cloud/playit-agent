use std::num::NonZeroU32;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_ipc::model::{LogEntry, LogLevel, ServiceUpdate};
use tokio::sync::broadcast;
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

pub const LOG_RATE_LIMIT_PER_SECOND: u32 = 2;
pub const LOG_RATE_LIMIT_BURST: u32 = 32;
const MAX_FIELD_LENGTH: usize = 256;
const MAX_MESSAGE_LENGTH: usize = 1_024;

struct MessageVisitor {
    message: String,
    include_fields: bool,
}

impl MessageVisitor {
    fn new(include_fields: bool) -> Self {
        Self {
            message: String::new(),
            include_fields,
        }
    }

    fn truncate(value: &str, max_chars: usize) -> String {
        if value.chars().count() <= max_chars {
            return value.to_string();
        }

        let mut value: String = value.chars().take(max_chars.saturating_sub(1)).collect();
        value.push('…');
        value
    }

    fn set_message(&mut self, value: String) {
        self.message = Self::truncate(&value, MAX_MESSAGE_LENGTH);
    }

    fn record_field(&mut self, name: &str, value: String) {
        if !self.include_fields {
            return;
        }

        if !self.message.is_empty() {
            self.message.push_str(", ");
        }
        let value = Self::truncate(&value, MAX_FIELD_LENGTH);
        self.message.push_str(name);
        self.message.push('=');
        self.message.push_str(&value);
        self.message = Self::truncate(&self.message, MAX_MESSAGE_LENGTH);
    }
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.set_message(format!("{value:?}"));
            return;
        }

        self.record_field(field.name(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.set_message(value.to_string());
            return;
        }

        self.record_field(field.name(), value.to_string());
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
    info_limiter: Arc<DefaultDirectRateLimiter>,
    debug_limiter: Arc<DefaultDirectRateLimiter>,
    suppressed: Arc<AtomicU64>,
}

impl LogRateLimiter {
    fn new() -> Self {
        let rate = NonZeroU32::new(LOG_RATE_LIMIT_PER_SECOND)
            .expect("log rate limit per second must be non-zero");
        let burst =
            NonZeroU32::new(LOG_RATE_LIMIT_BURST).expect("log rate limit burst must be non-zero");

        Self {
            info_limiter: Arc::new(RateLimiter::direct(
                Quota::per_second(rate).allow_burst(burst),
            )),
            debug_limiter: Arc::new(RateLimiter::direct(
                Quota::per_second(rate).allow_burst(burst),
            )),
            suppressed: Arc::new(AtomicU64::new(0)),
        }
    }

    fn allow_level(&self, level: &tracing::Level) -> bool {
        let allowed = match *level {
            tracing::Level::ERROR | tracing::Level::WARN => return true,
            tracing::Level::INFO => self.info_limiter.check().is_ok(),
            tracing::Level::DEBUG | tracing::Level::TRACE => self.debug_limiter.check().is_ok(),
        };
        if allowed {
            true
        } else {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    fn take_suppressed(&self) -> u64 {
        self.suppressed.swap(0, Ordering::AcqRel)
    }
}

/// Tracing layer that broadcasts log events via IPC.
pub struct IpcBroadcastLayer {
    event_tx: broadcast::Sender<ServiceUpdate>,
    limiter: LogRateLimiter,
}

impl IpcBroadcastLayer {
    pub fn new(event_tx: broadcast::Sender<ServiceUpdate>) -> Self {
        Self {
            event_tx,
            limiter: LogRateLimiter::new(),
        }
    }

    fn send_suppressed_summary(&self, timestamp: u64) {
        let suppressed = self.limiter.take_suppressed();
        if suppressed == 0 {
            return;
        }

        let _ = self.event_tx.send(ServiceUpdate::Log(LogEntry {
            level: LogLevel::Info,
            target: "playitd::logging".to_string(),
            message: format!("{suppressed} log messages suppressed (rate limit)"),
            timestamp,
        }));
    }
}

impl<S: Subscriber> Layer<S> for IpcBroadcastLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        use playit_agent_core::utils::now_milli;

        let metadata = event.metadata();
        if !self.limiter.allow_level(metadata.level()) {
            return;
        }

        let timestamp = now_milli();
        self.send_suppressed_summary(timestamp);

        let include_fields = matches!(
            *metadata.level(),
            tracing::Level::DEBUG | tracing::Level::TRACE
        );
        let mut visitor = MessageVisitor::new(include_fields);
        event.record(&mut visitor);

        let _ = self.event_tx.send(ServiceUpdate::Log(LogEntry {
            level: level_to_wire(metadata.level()),
            target: metadata.target().to_string(),
            message: visitor.message,
            timestamp,
        }));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use playit_ipc::model::ServiceUpdate;
    use tracing::Level;
    use tracing::subscriber::with_default;
    use tracing_subscriber::layer::SubscriberExt;

    use super::{IpcBroadcastLayer, LOG_RATE_LIMIT_BURST, LogRateLimiter, MessageVisitor};

    #[test]
    fn log_rate_limiter_allows_initial_burst() {
        let limiter = LogRateLimiter::new();

        for _ in 0..LOG_RATE_LIMIT_BURST {
            assert!(limiter.allow_level(&Level::INFO));
        }
    }

    #[test]
    fn log_rate_limiter_rejects_after_burst() {
        let limiter = LogRateLimiter::new();

        for _ in 0..LOG_RATE_LIMIT_BURST {
            assert!(limiter.allow_level(&Level::INFO));
        }

        assert!(!limiter.allow_level(&Level::INFO));
        assert_eq!(limiter.take_suppressed(), 1);
    }

    #[test]
    fn log_rate_limiter_refills_at_sustained_rate() {
        let limiter = LogRateLimiter::new();

        for _ in 0..LOG_RATE_LIMIT_BURST {
            assert!(limiter.allow_level(&Level::DEBUG));
        }
        assert!(!limiter.allow_level(&Level::DEBUG));

        std::thread::sleep(Duration::from_millis(1_100));

        let accepted = (0..4)
            .filter(|_| limiter.allow_level(&Level::DEBUG))
            .count();
        assert!((2..=3).contains(&accepted));
    }

    #[test]
    fn warn_and_error_bypass_rate_limit() {
        let limiter = LogRateLimiter::new();
        for _ in 0..LOG_RATE_LIMIT_BURST {
            assert!(limiter.allow_level(&Level::INFO));
        }
        assert!(limiter.allow_level(&Level::WARN));
        assert!(limiter.allow_level(&Level::ERROR));
    }

    #[test]
    fn debug_burst_does_not_starve_info() {
        let limiter = LogRateLimiter::new();
        for _ in 0..LOG_RATE_LIMIT_BURST {
            assert!(limiter.allow_level(&Level::DEBUG));
        }
        assert!(!limiter.allow_level(&Level::DEBUG));
        assert!(limiter.allow_level(&Level::INFO));
    }

    #[test]
    fn user_messages_omit_fields() {
        let mut visitor = MessageVisitor::new(false);
        visitor.set_message("Tunnel ready".to_string());
        visitor.record_field("details", "internal".to_string());
        assert_eq!(visitor.message, "Tunnel ready");
    }

    #[test]
    fn debug_fields_and_total_message_are_bounded() {
        let mut visitor = MessageVisitor::new(true);
        visitor.set_message("Debug event".to_string());
        visitor.record_field("details", "x".repeat(2_000));
        assert!(visitor.message.chars().count() <= super::MAX_MESSAGE_LENGTH);
        assert!(visitor.message.ends_with('…'));
    }

    #[test]
    fn ipc_layer_reports_suppressed_messages_before_priority_event() {
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(64);
        let subscriber = tracing_subscriber::registry().with(IpcBroadcastLayer::new(event_tx));

        with_default(subscriber, || {
            for index in 0..=LOG_RATE_LIMIT_BURST {
                tracing::debug!(index, "Debug event");
            }
            tracing::warn!("Priority event");
        });

        let mut messages = Vec::new();
        while let Ok(ServiceUpdate::Log(entry)) = event_rx.try_recv() {
            messages.push(entry.message);
        }
        assert!(
            messages
                .iter()
                .any(|message| message == "1 log messages suppressed (rate limit)")
        );
        assert_eq!(messages.last().unwrap(), "Priority event");
    }
}
