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
pub struct ProtocolInfo {
    pub ipc_version: u32,
    pub capabilities: Vec<String>,
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
    InvalidRequestType,
    AgentDisabledOverLimit,
    InvalidSecret,
    SecretPinned,
    ProvisioningUnavailable,
    SecretWriteFailed,
}

impl ServiceErrorCode {
    pub const ALL: [Self; 9] = [
        Self::Internal,
        Self::UnsupportedProtocol,
        Self::InvalidRequest,
        Self::InvalidRequestType,
        Self::AgentDisabledOverLimit,
        Self::InvalidSecret,
        Self::SecretPinned,
        Self::ProvisioningUnavailable,
        Self::SecretWriteFailed,
    ];

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidRequestType => "invalid_request_type",
            Self::AgentDisabledOverLimit => "agent_disabled_over_limit",
            Self::InvalidSecret => "invalid_secret",
            Self::SecretPinned => "secret_pinned",
            Self::ProvisioningUnavailable => "provisioning_unavailable",
            Self::SecretWriteFailed => "secret_write_failed",
        }
    }

    pub const fn fallback_retry(&self) -> &'static str {
        match self {
            Self::AgentDisabledOverLimit => "after_backoff",
            Self::InvalidSecret | Self::SecretWriteFailed => "user_initiated",
            Self::Internal => "user_initiated",
            _ => "never",
        }
    }

    pub const fn fallback_action(&self) -> &'static str {
        match self {
            Self::AgentDisabledOverLimit => "review_account",
            Self::InvalidSecret | Self::SecretWriteFailed => "provision_secret",
            Self::SecretPinned => "restart_with_configuration",
            Self::Internal => "restart_service",
            _ => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemMeaning {
    pub code: String,
    pub retry: String,
    pub action: String,
    pub retry_at_millis: Option<u64>,
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

impl ServiceError {
    pub fn meaning(&self) -> ProblemMeaning {
        let details = self.details.as_ref();
        ProblemMeaning {
            code: detail_string(details, "problem_code")
                .unwrap_or_else(|| self.code.as_str().to_owned()),
            retry: detail_string(details, "retry")
                .unwrap_or_else(|| self.code.fallback_retry().to_owned()),
            action: detail_string(details, "action")
                .unwrap_or_else(|| self.code.fallback_action().to_owned()),
            retry_at_millis: details
                .and_then(|value| value.get("retry_at_millis"))
                .and_then(serde_json::Value::as_u64),
        }
    }
}

fn detail_string(details: Option<&serde_json::Value>, key: &str) -> Option<String> {
    details?.get(key)?.as_str().map(str::to_owned)
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
pub struct ClaimSessionResponse {
    pub claim_code: String,
    pub claim_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimProgressResponse {
    WaitingForVisit,
    WaitingForApproval,
    Approved,
    Rejected,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum ClaimExchangeResponse {
    Pending(String),
    Accepted,
}

impl std::fmt::Debug for ClaimExchangeResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending(status) => formatter.debug_tuple("Pending").field(status).finish(),
            Self::Accepted => formatter.write_str("Accepted"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubscribeResponse {
    pub protocol: ProtocolInfo,
    pub snapshot: SubscriptionSnapshot,
}
