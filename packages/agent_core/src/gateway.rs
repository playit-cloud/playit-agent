use std::fmt;
use std::net::{IpAddr, SocketAddr};

use async_trait::async_trait;
use playit_model::{
    AccountStatus, Notice, PendingTunnel, Problem, ProblemCode, ProxyProtocol, TunnelCatalog,
    TunnelProtocol,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Freebsd,
    Windows,
    Macos,
    Android,
    Ios,
    Docker,
    MinecraftPlugin,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentVersion {
    pub variant_id: uuid::Uuid,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterRequest {
    pub proto_version: u64,
    pub version: AgentVersion,
    pub platform: Platform,
    pub client_addr: SocketAddr,
    pub tunnel_addr: SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedAgentKey {
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDataContext {
    pub catalog_revision: u64,
    pub accepted_at_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRunData {
    pub agent_id: String,
    pub origins: Vec<GatewayOrigin>,
    pub catalog: TunnelCatalog,
    pub account_status: AccountStatus,
    pub pending: Vec<PendingTunnel>,
    pub notices: Vec<Notice>,
    pub login_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayOrigin {
    pub tunnel_id: u64,
    pub protocol: TunnelProtocol,
    pub target: GatewayOriginTarget,
    pub port_count: u16,
    pub proxy_protocol: ProxyProtocol,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayOriginHost {
    Ip(IpAddr),
    Hostname(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayOriginTarget {
    Https {
        host: GatewayOriginHost,
        http_port: u16,
        https_port: u16,
    },
    Port {
        host: GatewayOriginHost,
        port: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayErrorCode {
    InvalidSecret,
    AgentDisabledOverLimit,
    Authentication,
    Transport,
    Rejected,
    InvalidRunData,
    NotReady,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayError {
    pub code: GatewayErrorCode,
    pub operation: &'static str,
    pub detail: String,
}

impl GatewayError {
    pub fn new(code: GatewayErrorCode, operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            operation,
            detail: detail.into(),
        }
    }

    pub const fn problem_code(&self) -> ProblemCode {
        match self.code {
            GatewayErrorCode::InvalidSecret => ProblemCode::InvalidSecret,
            GatewayErrorCode::AgentDisabledOverLimit => ProblemCode::AgentDisabledOverLimit,
            GatewayErrorCode::InvalidRunData => ProblemCode::CatalogInvalid,
            GatewayErrorCode::NotReady => ProblemCode::EngineUnavailable,
            GatewayErrorCode::Authentication
            | GatewayErrorCode::Transport
            | GatewayErrorCode::Rejected => ProblemCode::CatalogUnavailable,
        }
    }

    pub const fn problem(&self) -> Problem {
        Problem::new(self.problem_code())
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "playit gateway {} failed: {}",
            self.operation, self.detail
        )
    }
}

impl std::error::Error for GatewayError {}

#[async_trait]
pub trait PlayitGateway: Send + Sync {
    async fn register(&self, request: RegisterRequest) -> Result<SignedAgentKey, GatewayError>;
    async fn control_addresses(&self) -> Result<Vec<SocketAddr>, GatewayError>;
    async fn run_data(&self, context: RunDataContext) -> Result<GatewayRunData, GatewayError>;
    async fn account_login_url(&self) -> Result<String, GatewayError>;
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Default)]
    struct FakeGateway {
        calls: Mutex<Vec<&'static str>>,
    }

    #[async_trait]
    impl PlayitGateway for FakeGateway {
        async fn register(
            &self,
            _request: RegisterRequest,
        ) -> Result<SignedAgentKey, GatewayError> {
            self.calls.lock().unwrap().push("register");
            Ok(SignedAgentKey {
                key: "signed".to_owned(),
            })
        }

        async fn control_addresses(&self) -> Result<Vec<SocketAddr>, GatewayError> {
            self.calls.lock().unwrap().push("control_addresses");
            Ok(vec!["127.0.0.1:5525".parse().unwrap()])
        }

        async fn run_data(&self, _context: RunDataContext) -> Result<GatewayRunData, GatewayError> {
            Err(GatewayError::new(
                GatewayErrorCode::InvalidRunData,
                "run_data",
                "fixture",
            ))
        }

        async fn account_login_url(&self) -> Result<String, GatewayError> {
            self.calls.lock().unwrap().push("account_login_url");
            Ok("https://playit.gg/login/fixture".to_owned())
        }
    }

    #[tokio::test]
    async fn fake_gateway_satisfies_the_engine_contract() {
        let gateway = Arc::new(FakeGateway::default());
        assert_eq!(
            gateway.account_login_url().await.unwrap(),
            "https://playit.gg/login/fixture"
        );
        assert_eq!(
            gateway.control_addresses().await.unwrap(),
            vec!["127.0.0.1:5525".parse().unwrap()]
        );
        assert_eq!(
            gateway.calls.lock().unwrap().as_slice(),
            ["account_login_url", "control_addresses"]
        );
    }

    #[test]
    fn gateway_errors_have_stable_problem_codes() {
        let cases = [
            (GatewayErrorCode::InvalidSecret, ProblemCode::InvalidSecret),
            (
                GatewayErrorCode::AgentDisabledOverLimit,
                ProblemCode::AgentDisabledOverLimit,
            ),
            (
                GatewayErrorCode::InvalidRunData,
                ProblemCode::CatalogInvalid,
            ),
            (GatewayErrorCode::NotReady, ProblemCode::EngineUnavailable),
        ];
        for (gateway_code, problem_code) in cases {
            let error = GatewayError::new(gateway_code, "fixture", "fixture");
            assert_eq!(error.problem_code(), problem_code);
        }
    }
}
