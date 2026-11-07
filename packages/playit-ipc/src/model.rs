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
    /// Sent in place of the hello when the connecting user isn't allowed to use the daemon.
    AccessDenied,
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
    /// The tunnel's ID on playit.gg, which addresses its page on the website.
    pub id: String,
    pub display_address: String,
    pub destination: String,
    pub is_disabled: bool,
    pub disabled_reason: Option<String>,
    /// Set for HTTPS tunnels where playit terminates TLS with a Let's Encrypt certificate.
    /// The tunnel can't serve HTTPS until this is `Ready`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<CertificateState>,
}

impl TunnelState {
    /// True while the tunnel is waiting for its TLS certificate and can't serve HTTPS yet.
    pub fn is_pending(&self) -> bool {
        self.certificate
            .as_ref()
            .is_some_and(|certificate| !certificate.is_ready())
    }
}

/// Progress of the Let's Encrypt certificate for a tunnel that terminates TLS.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum CertificateState {
    /// Loading a stored certificate or waiting for the agent to connect.
    Preparing,
    /// Checking that Let's Encrypt will be able to reach the tunnel.
    CheckingReachability,
    Requesting,
    Validating,
    Issuing,
    /// Times are milliseconds since the Unix epoch.
    Ready {
        expires_at: u64,
        renewing: bool,
    },
    Failed {
        message: String,
        retry_at: u64,
    },
}

impl CertificateState {
    pub fn is_ready(&self) -> bool {
        matches!(self, CertificateState::Ready { .. })
    }

    /// A short description of what is happening, for status lines.
    pub fn progress_message(&self) -> String {
        match self {
            CertificateState::Preparing => "preparing TLS certificate".into(),
            CertificateState::CheckingReachability => {
                "checking the tunnel is reachable for Let's Encrypt".into()
            }
            CertificateState::Requesting => "requesting certificate from Let's Encrypt".into(),
            CertificateState::Validating => "Let's Encrypt is validating the domain".into(),
            CertificateState::Issuing => "issuing TLS certificate".into(),
            CertificateState::Ready { renewing: true, .. } => "renewing TLS certificate".into(),
            CertificateState::Ready { .. } => "TLS certificate ready".into(),
            CertificateState::Failed { message, .. } => {
                format!("could not get a TLS certificate: {message}")
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{CertificateState, TunnelState};

    #[test]
    fn certificate_state_is_optional_and_tagged() {
        let tunnel: TunnelState = serde_json::from_value(serde_json::json!({
            "id": "7c1e2a40-0f6b-4d2e-9a61-3b5f8e2d1c01",
            "display_address": "site.example.com",
            "destination": "127.0.0.1:80",
            "is_disabled": false,
            "disabled_reason": null,
        }))
        .unwrap();
        assert_eq!(tunnel.certificate, None);
        assert!(!tunnel.is_pending());

        let tunnel = TunnelState {
            certificate: Some(CertificateState::Failed {
                message: "unreachable".into(),
                retry_at: 5,
            }),
            ..tunnel
        };
        let json = serde_json::to_value(&tunnel).unwrap();
        assert_eq!(
            json["certificate"],
            serde_json::json!({"phase": "failed", "message": "unreachable", "retry_at": 5})
        );
        assert!(tunnel.is_pending());
        let ready = CertificateState::Ready {
            expires_at: 1,
            renewing: true,
        };
        assert!(ready.is_ready());
    }

    #[test]
    fn tunnel_state_carries_its_id() {
        let tunnel: TunnelState = serde_json::from_value(serde_json::json!({
            "id": "7c1e2a40-0f6b-4d2e-9a61-3b5f8e2d1c01",
            "display_address": "mc.example:25565",
            "destination": "127.0.0.1:25565",
            "is_disabled": false,
            "disabled_reason": null,
        }))
        .unwrap();
        assert_eq!(tunnel.id, "7c1e2a40-0f6b-4d2e-9a61-3b5f8e2d1c01");

        // Version 2 tunnels had no ID; version 3 requires one.
        assert!(
            serde_json::from_value::<TunnelState>(serde_json::json!({
                "display_address": "mc.example:25565",
                "destination": "127.0.0.1:25565",
                "is_disabled": false,
                "disabled_reason": null,
            }))
            .is_err()
        );
    }
}
