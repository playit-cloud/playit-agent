use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_ipc::model::{LogEntry, LogLevel, ServiceUpdate};
use tokio::sync::broadcast;
use tracing::{Event, Metadata, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::dynamic_filter_fn;
use tracing_subscriber::layer::Context;
use tracing_subscriber::layer::{Filter, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

use crate::errors::LoggingError;

#[cfg(target_os = "windows")]
const WINDOWS_LOG_MAX_FILE_SIZE_BYTES: u64 = 5 * 1024 * 1024;
#[cfg(target_os = "windows")]
const WINDOWS_LOG_MAX_TOTAL_FILES: usize = 3;
#[cfg(target_os = "windows")]
const WINDOWS_LOG_MAX_ROTATED_FILES: usize = WINDOWS_LOG_MAX_TOTAL_FILES - 1;

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

pub(crate) fn init_tracing(
    log_filter: EnvFilter,
    use_ansi: bool,
    event_tx: broadcast::Sender<ServiceUpdate>,
    log_path: Option<&Path>,
) -> Result<Option<WorkerGuard>, LoggingError> {
    match log_path {
        Some(path) => {
            let writer = log_file_writer(path)?;
            let (non_blocking, guard) = tracing_appender::non_blocking(writer);
            let _ = tracing_subscriber::registry()
                .with(log_filter)
                .with(
                    IpcBroadcastLayer::new(event_tx)
                        .and_then(
                            tracing_subscriber::fmt::layer()
                                .with_ansi(use_ansi)
                                .with_writer(non_blocking),
                        )
                        .with_filter(log_rate_limit_filter()),
                )
                .try_init();
            Ok(Some(guard))
        }
        None => {
            let _ = tracing_subscriber::registry()
                .with(log_filter)
                .with(
                    IpcBroadcastLayer::new(event_tx)
                        .and_then(
                            tracing_subscriber::fmt::layer()
                                .with_ansi(use_ansi)
                                .with_writer(std::io::stderr),
                        )
                        .with_filter(log_rate_limit_filter()),
                )
                .try_init();
            Ok(None)
        }
    }
}

#[cfg(target_os = "windows")]
fn log_file_writer(
    path: &Path,
) -> Result<tracing_rolling_file::RollingFileAppenderBase, LoggingError> {
    windows_log_file_writer_with_limits(
        path,
        WINDOWS_LOG_MAX_FILE_SIZE_BYTES,
        WINDOWS_LOG_MAX_ROTATED_FILES,
    )
}

#[cfg(target_os = "windows")]
pub(crate) fn windows_log_file_writer_with_limits(
    path: &Path,
    max_file_size_bytes: u64,
    max_rotated_files: usize,
) -> Result<tracing_rolling_file::RollingFileAppenderBase, LoggingError> {
    create_log_parent_dir(path)?;
    Ok(tracing_rolling_file::RollingFileAppenderBase::builder()
        .filename(path.display().to_string())
        .max_filecount(max_rotated_files)
        .condition_max_file_size(max_file_size_bytes)
        .build()
        .map_err(|error| {
            LoggingError(format!(
                "Failed to create log file writer {}: {error}",
                path.display()
            ))
        })?)
}

#[cfg(not(target_os = "windows"))]
fn log_file_writer(
    path: &Path,
) -> Result<tracing_appender::rolling::RollingFileAppender, LoggingError> {
    create_log_parent_dir(path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|file| file.to_str())
        .ok_or_else(|| LoggingError(format!("Invalid --log-path {}", path.display())))?;
    Ok(tracing_appender::rolling::never(parent, file_name))
}

fn create_log_parent_dir(path: &Path) -> Result<(), LoggingError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| {
        LoggingError(format!(
            "Failed to create log directory {}: {error}",
            parent.display()
        ))
    })
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
