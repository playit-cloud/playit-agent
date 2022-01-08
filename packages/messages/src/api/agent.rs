use std::fmt::{Debug, Formatter};
use std::net::SocketAddr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentRegistered, TunnelRequest};
use crate::rpc::SignedRpcRequest;

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
pub enum AgentApiRequest {
    #[serde(rename = "get-control-address")]
    GetControlAddr,

    #[serde(rename = "sign-tunnel-request")]
    SignControlRequest(TunnelRequest),

    #[serde(rename = "generate-session-secret")]
    GenerateSharedTunnelSecret(AgentRegistered),

    #[serde(rename = "get-tunnel-server-details")]
    GetTunnelServerDetails(GetTunnelServerDetails),
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

}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SessionSecret {
    pub agent_registered: AgentRegistered,
    pub secret: String,
}

impl Debug for SessionSecret {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionSecret {{ agent_registered: {:?}, secret: <redacted> }}", self.agent_registered)
    }
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
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