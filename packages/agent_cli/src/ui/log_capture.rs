use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::service::ipc::ServiceEvent;

/// A log entry captured from tracing
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub timestamp: u64,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

impl From<&tracing::Level> for LogLevel {
    fn from(level: &tracing::Level) -> Self {
        match *level {
            tracing::Level::TRACE => LogLevel::Trace,
            tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::ERROR => LogLevel::Error,
        }
    }
}

/// Ring buffer for captured log entries
pub struct LogCapture {
    entries: Mutex<VecDeque<LogEntry>>,
    capacity: usize,
}

impl LogCapture {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(LogCapture {
            entries: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        })
    }

    /// Add a log entry to the buffer
    pub fn push(&self, entry: LogEntry) {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= self.capacity {
            entries.pop_front();
        }
        entries.push_back(entry);
    }

    /// Get a snapshot of all log entries
    pub fn get_entries(&self) -> Vec<LogEntry> {
        let entries = self.entries.lock().unwrap();
        entries.iter().cloned().collect()
    }

    /// Get the number of log entries
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    /// Check if the log buffer is empty
    pub fn is_empty(&self) -> bool {
        self.entries.lock().unwrap().is_empty()
    }

    /// Clear all log entries
    #[allow(dead_code)]
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }
}

/// Tracing layer that captures log events into a LogCapture buffer
pub struct LogCaptureLayer {
    capture: Arc<LogCapture>,
}

impl LogCaptureLayer {
    pub fn new(capture: Arc<LogCapture>) -> Self {
        LogCaptureLayer { capture }
    }
}

impl<S: Subscriber> Layer<S> for LogCaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        use playit_agent_core::utils::now_milli;

        let metadata = event.metadata();
        let level = LogLevel::from(metadata.level());
        let target = metadata.target().to_string();

        // Extract the message from the event
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let entry = LogEntry {
            timestamp: now_milli(),
            level,
            target,
            message: visitor.message,
        };

        self.capture.push(entry);
    }
}

/// Visitor to extract the message field from a tracing event
#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        } else if self.message.is_empty() {
            // Fallback: use any debug value if no message field
            if !self.message.is_empty() {
                self.message.push_str(", ");
            }
            self.message.push_str(&format!("{}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            if !self.message.is_empty() {
                self.message.push_str(", ");
            }
            self.message.push_str(&format!("{}={}", field.name(), value));
        }
    }
}

/// Tracing layer that broadcasts log events via IPC
pub struct IpcBroadcastLayer {
    event_tx: broadcast::Sender<ServiceEvent>,
}

impl IpcBroadcastLayer {
    pub fn new(event_tx: broadcast::Sender<ServiceEvent>) -> Self {
        IpcBroadcastLayer { event_tx }
    }
}

impl<S: Subscriber> Layer<S> for IpcBroadcastLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        use playit_agent_core::utils::now_milli;

        let metadata = event.metadata();
        let level = LogLevel::from(metadata.level());
        let target = metadata.target().to_string();

        // Extract the message from the event
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let log_event = ServiceEvent::Log {
            level: level.as_str().to_string(),
            target,
            message: visitor.message,
            timestamp: now_milli(),
        };

        // Ignore send errors (no subscribers connected)
        let _ = self.event_tx.send(log_event);
    }
}
