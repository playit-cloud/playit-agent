use std::collections::VecDeque;
use std::time::Instant;

use chrono::{DateTime, Local, NaiveDate};
use playit_ipc::model::{
    AgentLifecycle, AgentState, ConnectionStats, LogEntry, LogLevel, ServiceStatus, ServiceUpdate,
    SubscribeResponse,
};

use crate::backend::{BackendEvent, SetupStep};

const LOG_CAPACITY: usize = 500;
pub const ACCOUNT_URL: &str = "https://playit.gg/account";
pub const ACCOUNT_AGENTS_URL: &str = "https://playit.gg/account/agents";
pub const ACCOUNT_TUNNELS_URL: &str = "https://playit.gg/account/tunnels";
pub const ACCOUNT_UPGRADE_URL: &str = "https://playit.gg/account/upgrade";
const NEW_TUNNEL_URL: &str = "https://playit.gg/account/setup/new-tunnel";

/// The website's page for one tunnel, or the tunnel list when the daemon didn't send an ID.
pub fn tunnel_url(tunnel_id: &str) -> String {
    if tunnel_id.is_empty() {
        ACCOUNT_TUNNELS_URL.to_string()
    } else {
        format!("{ACCOUNT_TUNNELS_URL}/{tunnel_id}")
    }
}

/// The website's new-tunnel page, preset to point the tunnel at this agent when its ID is known.
pub fn new_tunnel_url(agent_id: &str) -> String {
    if agent_id.is_empty() {
        NEW_TUNNEL_URL.to_string()
    } else {
        format!("{NEW_TUNNEL_URL}?agent_id={agent_id}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMode {
    Service,
    ExternalDaemon,
}

#[derive(Clone, Debug)]
pub enum Connection {
    Connecting,
    Unreachable { error: String },
    Connected,
}

#[derive(Clone, Debug)]
pub struct SetupState {
    pub url: Option<String>,
    pub step: SetupStep,
}

/// What the window shows, derived from the connection and the agent lifecycle.
pub enum Screen<'a> {
    Connecting,
    Unreachable { error: &'a str },
    Setup(&'a SetupState),
    InvalidSecret { message: &'a str },
    OverLimit { message: &'a str },
    Starting,
    Stopping,
    Error { message: &'a str },
    Running(&'a AgentState),
}

pub struct AppState {
    pub socket_path: String,
    pub session_mode: SessionMode,
    pub connection: Connection,
    pub status: Option<ServiceStatus>,
    pub lifecycle: AgentLifecycle,
    pub stats: ConnectionStats,
    pub logs: VecDeque<LogEntry>,
    pub setup: Option<SetupState>,
    /// When the agent entered its current lifecycle state, to notice slow starts.
    pub lifecycle_since: Instant,
}

impl AppState {
    pub fn new(socket_path: String, session_mode: SessionMode) -> Self {
        Self {
            socket_path,
            session_mode,
            connection: Connection::Connecting,
            status: None,
            lifecycle: AgentLifecycle::Starting,
            stats: ConnectionStats::default(),
            logs: VecDeque::with_capacity(LOG_CAPACITY),
            setup: None,
            lifecycle_since: Instant::now(),
        }
    }

    pub fn apply(&mut self, event: BackendEvent) {
        match event {
            BackendEvent::Subscribed(subscribe) => {
                let SubscribeResponse { snapshot, .. } = *subscribe;
                self.connection = Connection::Connected;
                self.status = Some(snapshot.status);
                self.set_lifecycle(snapshot.lifecycle);
                self.stats = snapshot.stats;
            }
            BackendEvent::Update(update) => match update {
                ServiceUpdate::Status(status) => self.status = Some(status),
                ServiceUpdate::Lifecycle(lifecycle) => self.set_lifecycle(lifecycle),
                ServiceUpdate::Stats(stats) => self.stats = stats,
                ServiceUpdate::Log(entry) => self.push_log(entry),
            },
            BackendEvent::Unreachable { error } => {
                self.connection = Connection::Unreachable { error };
            }
            BackendEvent::Setup { url, step } => {
                if let Some(setup) = &mut self.setup {
                    if url.is_some() {
                        setup.url = url;
                    }
                    setup.step = step;
                }
            }
        }
    }

    fn set_lifecycle(&mut self, lifecycle: AgentLifecycle) {
        if !matches!(lifecycle, AgentLifecycle::WaitingForSecret) {
            self.setup = None;
        }
        if std::mem::discriminant(&lifecycle) != std::mem::discriminant(&self.lifecycle) {
            self.lifecycle_since = Instant::now();
        }
        self.lifecycle = lifecycle;
    }

    fn push_log(&mut self, entry: LogEntry) {
        if self.logs.len() >= LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(user_facing_log(entry));
    }

    /// The daemon is waiting for a secret and no claim is in progress yet.
    pub fn needs_setup(&self) -> bool {
        matches!(self.connection, Connection::Connected)
            && matches!(self.lifecycle, AgentLifecycle::WaitingForSecret)
            && self.setup.is_none()
    }

    pub fn begin_setup(&mut self) {
        self.setup = Some(SetupState {
            url: None,
            step: SetupStep::Preparing,
        });
    }

    pub fn screen(&self) -> Screen<'_> {
        match &self.connection {
            Connection::Connecting => return Screen::Connecting,
            Connection::Unreachable { error } => return Screen::Unreachable { error },
            Connection::Connected => {}
        }

        match &self.lifecycle {
            AgentLifecycle::WaitingForSecret => match &self.setup {
                Some(setup) => Screen::Setup(setup),
                None => Screen::Starting,
            },
            AgentLifecycle::HasInvalidSecret(error) => Screen::InvalidSecret {
                message: &error.message,
            },
            AgentLifecycle::DisabledOverLimit(error) => Screen::OverLimit {
                message: &error.message,
            },
            AgentLifecycle::Starting => Screen::Starting,
            AgentLifecycle::Running(state) => Screen::Running(state),
            AgentLifecycle::Stopping => Screen::Stopping,
            AgentLifecycle::Error(error) => Screen::Error {
                message: &error.message,
            },
        }
    }

    pub fn logs_text(&self) -> String {
        let today = Local::now().date_naive();
        self.logs
            .iter()
            .map(|entry| {
                format!(
                    "{} [{}] {}: {}\n",
                    log_time_label(entry.timestamp, today),
                    level_label(&entry.level),
                    short_target(&entry.target),
                    entry.message
                )
            })
            .collect()
    }
}

pub fn level_label(level: &LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "TRACE",
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERROR",
    }
}

pub fn short_target(target: &str) -> &str {
    target.rsplit("::").next().unwrap_or(target)
}

/// Formats a log timestamp in local time, including the date only when it is not `today`.
pub fn log_time_label(timestamp_millis: u64, today: NaiveDate) -> String {
    let Some(time) = DateTime::from_timestamp_millis(timestamp_millis as i64) else {
        return "--:--:--".to_string();
    };
    let time = time.with_timezone(&Local);
    if time.date_naive() == today {
        time.format("%H:%M:%S").to_string()
    } else {
        time.format("%Y-%m-%d %H:%M:%S").to_string()
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

pub fn format_uptime(start_time_millis: u64, now_millis: u64) -> String {
    let elapsed_secs = now_millis.saturating_sub(start_time_millis) / 1000;
    let hours = elapsed_secs / 3600;
    let minutes = (elapsed_secs % 3600) / 60;
    let seconds = elapsed_secs % 60;

    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// Explains why a tunnel is disabled as a sentence, without a redundant "disabled" prefix.
pub fn disabled_reason(reason: Option<&str>) -> String {
    let reason = reason.unwrap_or_default().trim();
    let lower = reason.to_ascii_lowercase();
    let reason = match lower.strip_prefix("disabled") {
        Some(rest) => reason[reason.len() - rest.len()..].trim_start_matches([':', ' ', '-']),
        None => reason,
    };
    let mut chars = reason.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Turned off in your playit account".to_string(),
    }
}

fn user_facing_log(mut entry: LogEntry) -> LogEntry {
    if entry.target == "playit_agent_core::agent_control::maintained_control"
        && entry
            .message
            .starts_with("control session expired; reconnecting")
    {
        entry.message = if entry.message.contains("reason=SessionNotSetup") {
            "Connecting to playit..."
        } else {
            "Connection interrupted; reconnecting..."
        }
        .to_string();
        entry.target = "playit".to_string();
        entry.level = LogLevel::Info;
    }
    entry
}

#[cfg(test)]
mod tests {
    use playit_ipc::model::{ServiceError, SubscriptionSnapshot};

    use super::*;

    fn connected(lifecycle: AgentLifecycle) -> AppState {
        let mut state = AppState::new("test.sock".into(), SessionMode::Service);
        state.apply(BackendEvent::Subscribed(Box::new(SubscribeResponse {
            protocol: Default::default(),
            snapshot: SubscriptionSnapshot {
                lifecycle,
                ..Default::default()
            },
        })));
        state
    }

    #[test]
    fn setup_starts_once_when_waiting_for_secret() {
        let mut state = connected(AgentLifecycle::WaitingForSecret);
        assert!(state.needs_setup());
        state.begin_setup();
        assert!(!state.needs_setup());
        assert!(matches!(state.screen(), Screen::Setup(_)));
    }

    #[test]
    fn setup_clears_once_the_agent_has_a_secret() {
        let mut state = connected(AgentLifecycle::WaitingForSecret);
        state.begin_setup();
        state.apply(BackendEvent::Update(ServiceUpdate::Lifecycle(
            AgentLifecycle::Starting,
        )));
        assert!(state.setup.is_none());
        assert!(matches!(state.screen(), Screen::Starting));
    }

    #[test]
    fn lost_connection_hides_agent_state() {
        let mut state = connected(AgentLifecycle::Error(ServiceError {
            message: "boom".into(),
            ..Default::default()
        }));
        assert!(matches!(state.screen(), Screen::Error { message: "boom" }));
        state.apply(BackendEvent::Unreachable {
            error: "gone".into(),
        });
        assert!(matches!(
            state.screen(),
            Screen::Unreachable { error: "gone" }
        ));
    }

    #[test]
    fn reconnection_logs_are_explained() {
        let entry = user_facing_log(LogEntry {
            level: LogLevel::Warn,
            target: "playit_agent_core::agent_control::maintained_control".into(),
            message: "control session expired; reconnecting, reason=FlowChanged".into(),
            timestamp: 1,
        });
        assert_eq!(entry.message, "Connection interrupted; reconnecting...");
        assert!(matches!(entry.level, LogLevel::Info));
    }

    #[test]
    fn formats_values_like_the_terminal_ui() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_uptime(0, 3_723_000), "1h 02m 03s");
        assert_eq!(format_uptime(0, 61_000), "1m 01s");
        assert_eq!(
            disabled_reason(Some("over tunnel limit")),
            "Over tunnel limit"
        );
        assert_eq!(disabled_reason(Some("Disabled: by user")), "By user");
        assert_eq!(disabled_reason(None), "Turned off in your playit account");
        assert_eq!(short_target("a::b::c"), "c");
        assert_eq!(
            new_tunnel_url("a3f1"),
            "https://playit.gg/account/setup/new-tunnel?agent_id=a3f1"
        );
        assert_eq!(tunnel_url("9f2c"), "https://playit.gg/account/tunnels/9f2c");
        assert_eq!(tunnel_url(""), "https://playit.gg/account/tunnels");
        assert_eq!(
            new_tunnel_url(""),
            "https://playit.gg/account/setup/new-tunnel"
        );
    }
}
