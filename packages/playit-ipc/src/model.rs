use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    #[default]
    Unknown,
    Guest,
    EmailNotVerified,
    Verified,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceCapability {
    #[default]
    StructuredResponses,
    StreamEvents,
    LifecycleState,
    RichStatus,
    SecretProvisioning,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProtocolInfo {
    pub version: u32,
    pub capabilities: Vec<ServiceCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServicePhase {
    WaitingForSecret,
    HasInvalidSecret,
    DisabledOverLimit,
    #[default]
    Starting,
    Running,
    Stopping,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceErrorCode {
    #[default]
    Internal,
    UnsupportedProtocol,
    InvalidRequest,
    AgentDisabledOverLimit,
    InvalidSecret,
    SecretPinned,
    ProvisioningUnavailable,
    SecretWriteFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceError {
    pub code: ServiceErrorCode,
    pub message: String,
    pub retryable: bool,
    pub details: Option<serde_json::Value>,
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TunnelState {
    pub display_address: String,
    pub destination: String,
    pub is_disabled: bool,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PendingTunnelState {
    pub id: String,
    pub status_msg: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NoticeState {
    pub priority: String,
    pub message: String,
    pub resolve_link: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentState {
    pub version: String,
    pub tunnels: Vec<TunnelState>,
    pub pending_tunnels: Vec<PendingTunnelState>,
    pub notices: Vec<NoticeState>,
    pub account_status: AccountStatus,
    pub agent_id: String,
    pub login_link: Option<String>,
    pub start_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum AgentLifecycle {
    WaitingForSecret,
    HasInvalidSecret(ServiceError),
    DisabledOverLimit(ServiceError),
    #[default]
    Starting,
    Running(AgentState),
    Stopping,
    Error(ServiceError),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionStats {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_tcp: u32,
    pub active_udp: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceStatus {
    pub phase: ServicePhase,
    pub pid: u32,
    pub uptime_secs: u64,
    pub version: String,
    pub socket_path: String,
    pub secret_path: Option<String>,
    pub has_secret: bool,
    pub protocol: ProtocolInfo,
    pub last_error: Option<ServiceError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LogEntry {
    pub level: LogLevel,
    pub target: String,
    pub message: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubscriptionSnapshot {
    pub status: ServiceStatus,
    pub lifecycle: AgentLifecycle,
    pub stats: ConnectionStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServiceUpdate {
    Status(ServiceStatus),
    Lifecycle(AgentLifecycle),
    Stats(ConnectionStats),
    Log(LogEntry),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommandResponse {
    pub accepted: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SecretPathResponse {
    pub secret_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountLoginUrlResponse {
    pub login_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubscribeResponse {
    pub protocol: ProtocolInfo,
    pub snapshot: SubscriptionSnapshot,
}
