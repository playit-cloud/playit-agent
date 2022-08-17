use std::fmt::{Debug, Formatter};
use std::net::SocketAddr;

#[cfg(feature = "use-schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentRegistered, TunnelRequest};
use crate::agent_config::AgentConfigBuilder;
use crate::rpc::SignedRpcRequest;

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
#[non_exhaustive]
#[serde(tag = "type")]
pub enum AgentApiRequest {
    #[serde(rename = "get-control-address")]
    GetControlAddr,

    #[serde(rename = "get-agent-account-status")]
    GetAgentAccountStatus { client_version: Option<String> },

    #[serde(rename = "sign-tunnel-request")]
    SignControlRequest(TunnelRequest),

    #[serde(rename = "generate-session-secret")]
    GenerateSharedTunnelSecret(AgentRegistered),

    #[serde(rename = "get-tunnel-server-details")]
    GetTunnelServerDetails(GetTunnelServerDetails),

    #[serde(rename = "exchange-claim-for-secret")]
    ExchangeClaimForSecret(ExchangeClaimForSecret),

    #[serde(rename = "get-agent-config")]
    GetAgentConfig {
        client_version: Option<String>,
    },
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum AgentApiResponse {
    #[serde(rename = "control-address")]
    ControlAddress(ControlAddress),

    #[serde(rename = "signed-tunnel-request")]
    SignedTunnelRequest(SignedRpcRequest<TunnelRequest>),

    #[serde(rename = "session-secret")]
    SessionSecret(SessionSecret),

    #[serde(rename = "tunnel-server-details")]
    TunnelServerDetails(TunnelServerDetails),

    #[serde(rename = "agent-secret")]
    AgentSecret(AgentSecret),

    #[serde(rename = "agent-config")]
    AgentConfig(AgentConfigBuilder),

    #[serde(rename = "agent-account-status")]
    AgentAccountStatus(AgentAccountStatus),
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "status")]
pub enum AgentAccountStatus {
    #[serde(rename = "no-account")]
    NoAccount {},
    #[serde(rename = "guest-account")]
    GuestAccount {
        account_id: u64,
        web_session_key: String,
    },
    #[serde(rename = "unverified-account")]
    UnverifiedAccount {
        account_id: u64
    },
    #[serde(rename = "verified-account")]
    VerifiedAccount {
        account_id: u64
    },
    #[serde(rename = "user-notice")]
    UserNotice {
        message: String,
        notice_url: String,
        important: bool,
        prevent_usage: bool,
    },
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize)]
pub struct SessionSecret {
    pub agent_registered: AgentRegistered,
    pub secret: String,
}

impl Debug for SessionSecret {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SessionSecret {{ agent_registered: {:?}, secret: <redacted> }}",
            self.agent_registered
        )
    }
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
pub struct ControlAddress {
    pub control_address: SocketAddr,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GetTunnelServerDetails {
    pub tunnel_server_id: u64,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TunnelServerDetails {
    pub name: String,
    pub datacenter_name: String,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExchangeClaimForSecret {
    pub claim_key: String,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentSecret {
    pub secret_key: String,
}