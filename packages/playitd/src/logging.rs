use playit_ipc::model::{LogEntry, LogLevel, ServiceUpdate};
use tokio::sync::broadcast;
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

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
