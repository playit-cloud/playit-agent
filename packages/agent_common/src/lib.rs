use std::net::{Ipv4Addr, SocketAddrV4};
use std::ops::Sub;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;

use crate::auth::SignatureError;

pub mod api;
pub mod auth;
pub mod rpc;
pub mod udp;
pub mod agent_config;

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct RpcMessage<T> {
    pub request_id: u64,
    pub content: T,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum TunnelFeed {
    Response(RpcMessage<TunnelResponse>),
    NewClient(NewClient),
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct NewClient {
    pub connect_addr: SocketAddrV4,
    pub peer_addr: SocketAddrV4,
    pub claim_instructions: ClaimInstructions,
    pub from_tunnel_server: String,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum ClaimInstructions {
    Tcp {
        address: SocketAddrV4,
        token: Vec<u8>,
    },
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum TunnelRequest {
    Ping(Ping),
    RegisterAgent,
    KeepAlive,
    ClaimLease(ClaimLease),
    SetupUdpChannel,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum TunnelResponse {
    AgentRegistered(AgentRegistered),
    ClaimResponse(Result<ClaimLease, ClaimError>),
    KeptAlive(KeptAlive),
    Pong(Pong),
    SignatureError(SignatureError),
    SetupUdpChannelDetails(SetupUdpChannelDetails),
    BadRequest,
    Failed,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct SetupUdpChannelDetails {
    pub tunnel_addr: SocketAddrV4,
    pub token: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum ClaimError {
    NotRegistered,
    LeaseConflict,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct ClaimLease {
    pub ip: Ipv4Addr,
    pub from_port: u16,
    pub to_port: u16,
    pub proto: ClaimProto,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Eq, PartialEq, JsonSchema)]
pub enum ClaimProto {
    #[serde(rename = "udp")]
    Udp,
    #[serde(rename = "tcp")]
    Tcp,
    #[serde(rename = "both")]
    Both,
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, JsonSchema)]
pub enum Proto {
    #[serde(rename = "udp")]
    Udp,
    #[serde(rename = "tcp")]
    Tcp,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub struct AgentRegistered {
    pub account_id: u64,
    pub session_id: u64,
    pub session_timestamp: u64,
    pub signature: [u8; 32],
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct Ping {
    pub id: u64,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct Pong {
    pub id: u64,
    pub tunnel_server_id: u64,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct KeptAlive {
    pub alive: bool,
    pub tunnel_server_id: u64,
}

impl<T: DeserializeOwned + Serialize> RpcMessage<T> {
    pub fn from_slice(slice: &[u8]) -> Option<Self> {
        bincode::deserialize(slice).ok()
    }

    pub fn as_payload(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }
}

impl TunnelFeed {
    pub fn from_slice(slice: &[u8]) -> Option<Self> {
        bincode::deserialize(slice).ok()
    }

    pub fn as_payload(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }
}

pub fn abs_diff<T: Ord + Sub<Output=T>>(a: T, b: T) -> T {
    if a > b {
        a - b
    } else {
        b - a
    }
}
