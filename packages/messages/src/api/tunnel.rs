use crate::Proto;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
pub enum TunnelApiRequest {
  #[serde(rename = "get-tunnel-network")]
  GetTunnelNetwork(GetTunnelNetwork),
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct GetTunnelNetwork {
  pub account_id: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
pub enum TunnelApiResponse {
  #[serde(rename = "tunnel-network")]
  TunnelNetwork(TunnelNetwork),
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct TunnelNetwork {
  pub agents: Vec<AgentConnection>,
  pub clients: Vec<ClientConnection>,
  pub port_leases: Vec<PortLease>,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct PortLease {
  pub proto: Proto,
  pub from_port: u16,
  pub to_port: u16,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct AgentConnection {
  pub tunnel_server: TunnelServer,
  pub control_flow: (SocketAddr, SocketAddr),
  pub udp_flow: Option<(SocketAddr, SocketAddr)>,
  pub last_activity: u64,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct ClientConnection {
  pub proto: Proto,
  pub agent_flow: (SocketAddr, SocketAddr),
  pub client_flow: (SocketAddr, SocketAddr),
  pub client_tunnel: TunnelServer,
  pub agent_tunnel: TunnelServer,
  pub started_at: u64,
  pub usage: ConnectionUsage,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct ConnectionUsage {
  pub client_packets: u64,
  pub client_bytes: u64,
  pub client_last_use: u64,

  pub agent_packets: u64,
  pub agent_bytes: u64,
  pub agent_last_use: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub struct TunnelServer {
  pub tunnel_id: u64,
  pub tunnel_name: String,
  pub datacenter: String,
  pub datacenter_id: u64,
}
