use std::fmt::{Debug, Formatter};
use std::net::SocketAddr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentRegistered, TunnelRequest};
use crate::agent_config::AgentConfig;
use crate::rpc::SignedRpcRequest;

#[derive(Serialize, Deserialize, Debug, JsonSchema, Clone)]
#[non_exhaustive]
#[serde(tag = "type")]
pub enum AgentApiRequest {
    #[serde(rename = "get-control-address")]
    GetControlAddr,

    #[serde(rename = "get-agent-account-status")]
    GetAgentAccountStatus,

    #[serde(rename = "sign-tunnel-request")]
    SignControlRequest(TunnelRequest),

    #[serde(rename = "generate-session-secret")]
    GenerateSharedTunnelSecret(AgentRegistered),

    #[serde(rename = "get-tunnel-server-details")]
    GetTunnelServerDetails(GetTunnelServerDetails),

    #[serde(rename = "exchange-claim-for-secret")]
    ExchangeClaimForSecret(ExchangeClaimForSecret),

    #[serde(rename = "get-agent-config")]
    GetAgentConfig,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
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
    AgentConfig(AgentConfig),

    #[serde(rename = "agent-account-status")]
    AgentAccountStatus(AgentAccountStatus),
}

#[derive(Serialize, Deserialize, Debug, JsonSchema, Clone)]
#[serde(tag = "status")]
pub enum AgentAccountStatus {
    #[serde(rename = "no-account")]
    NoAccount {
    },
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
}

#[derive(Serialize, Deserialize, JsonSchema, Clone)]
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

#[derive(Serialize, Deserialize, Debug, JsonSchema, Clone)]
pub struct ControlAddress {
    pub control_address: SocketAddr,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub struct GetTunnelServerDetails {
    pub tunnel_server_id: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub struct TunnelServerDetails {
    pub name: String,
    pub datacenter_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub struct ExchangeClaimForSecret {
    pub claim_key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub struct AgentSecret {
    pub secret_key: String,
}
