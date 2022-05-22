use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::ops::Sub;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::auth::SignatureError;

pub mod agent_config;
pub mod api;
pub mod auth;
pub mod hmac;
pub mod rpc;
pub mod udp;

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct RpcMessage<T> {
    pub request_id: u64,
    pub content: T,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum TunnelFeed {
    Response(RpcMessage<TunnelResponse>),
    NewClientV4(NewClientV4),
    NewClientV6(NewClientV6),
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct NewClientV4 {
    pub connect_addr: SocketAddrV4,
    pub peer_addr: SocketAddrV4,
    pub claim_instructions: ClaimInstructionVersioned,
    pub from_tunnel_server: String,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct NewClientV6 {
    pub connect_addr: SocketAddrV6,
    pub peer_addr: SocketAddrV6,
    pub claim_instructions: ClaimInstructionVersioned,
    pub from_tunnel_server: String,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct NewClient {
    pub connect_addr: SocketAddr,
    pub peer_addr: SocketAddr,
    pub claim_instructions: ClaimInstructionVersioned,
    pub from_tunnel_server: String,
}

impl From<NewClientV4> for NewClient {
    fn from(from: NewClientV4) -> Self {
        NewClient {
            connect_addr: SocketAddr::V4(from.connect_addr),
            peer_addr: SocketAddr::V4(from.peer_addr),
            claim_instructions: from.claim_instructions,
            from_tunnel_server: from.from_tunnel_server,
        }
    }
}

impl From<NewClientV6> for NewClient {
    fn from(from: NewClientV6) -> Self {
        NewClient {
            connect_addr: SocketAddr::V6(from.connect_addr),
            peer_addr: SocketAddr::V6(from.peer_addr),
            claim_instructions: from.claim_instructions,
            from_tunnel_server: from.from_tunnel_server,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum ClaimInstructionVersioned {
    Tcp4 {
        address: SocketAddrV4,
        token: Vec<u8>,
    },
    Tcp6 {
        address: SocketAddrV6,
        token: Vec<u8>,
    },
}

impl ClaimInstructionVersioned {
    pub fn into_instruction(self) -> ClaimInstruction {
        match self {
            Self::Tcp4 { address, token } => ClaimInstruction {
                address: address.into(),
                token,
            },
            Self::Tcp6 { address, token } => ClaimInstruction {
                address: address.into(),
                token,
            },
        }
    }
}

pub struct ClaimInstruction {
    pub address: SocketAddr,
    pub token: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize, Debug, JsonSchema)]
pub enum TunnelRequest {
    Ping(Ping),
    RegisterAgent,
    KeepAlive,
    ClaimLease(ClaimLeaseV4),
    SetupUdpChannel,
    ClaimLeaseV2(ClaimLease),
}

impl TunnelRequest {
    pub fn upgrade(self) -> TunnelRequest {
        match self {
            Self::ClaimLease(claim) => TunnelRequest::ClaimLeaseV2(claim.into()),
            req => req,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum TunnelResponse {
    AgentRegistered(AgentRegistered),
    ClaimResponse(Result<ClaimLeaseV4, ClaimError>),
    KeptAlive(KeptAlive),
    Pong(Pong),
    SignatureError(SignatureError),
    SetupUdpChannelDetails(SetupUdpChannelDetailsV4),
    BadRequest,
    Failed,
    SetupUdpChannelDetailsV6(SetupUdpChannelDetailsV6),
    ClaimResponseV2(Result<ClaimLease, ClaimError>),
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct SetupUdpChannelDetailsV4 {
    pub tunnel_addr: SocketAddrV4,
    pub token: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct SetupUdpChannelDetailsV6 {
    pub tunnel_addr: SocketAddrV6,
    pub token: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct SetupUdpChannelDetails {
    pub tunnel_addr: SocketAddr,
    pub token: Vec<u8>,
}

impl From<SetupUdpChannelDetailsV4> for SetupUdpChannelDetails {
    fn from(details: SetupUdpChannelDetailsV4) -> Self {
        SetupUdpChannelDetails {
            tunnel_addr: details.tunnel_addr.into(),
            token: details.token,
        }
    }
}

impl From<SetupUdpChannelDetailsV6> for SetupUdpChannelDetails {
    fn from(details: SetupUdpChannelDetailsV6) -> Self {
        SetupUdpChannelDetails {
            tunnel_addr: details.tunnel_addr.into(),
            token: details.token,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum ClaimError {
    NotRegistered,
    LeaseConflict,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct ClaimLeaseV4 {
    pub ip: Ipv4Addr,
    pub from_port: u16,
    pub to_port: u16,
    pub proto: ClaimProto,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct ClaimLease {
    pub ip: IpAddr,
    pub from_port: u16,
    pub to_port: u16,
    pub proto: ClaimProto,
}

impl From<ClaimLeaseV4> for ClaimLease {
    fn from(claim: ClaimLeaseV4) -> Self {
        ClaimLease {
            ip: claim.ip.into(),
            from_port: claim.from_port,
            to_port: claim.to_port,
            proto: claim.proto,
        }
    }
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

#[derive(Clone, Serialize, Deserialize, Debug, JsonSchema)]
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

pub fn abs_diff<T: Ord + Sub<Output = T>>(a: T, b: T) -> T {
    if a > b {
        a - b
    } else {
        b - a
    }
}

#[cfg(test)]
mod test {
    use crate::{TunnelFeed, TunnelRequest};

    #[test]
    fn test_pares_tunnel_requests() {
        let hex = "0700000000000000030000007f00000101002c0100000000030000007f00000101002c0101000000030000007f00000101002c0102000000000000001f00000000000000020000000100000004000000";
        let bytes = hex::decode(hex).unwrap();
        let parsed: Vec<TunnelRequest> = bincode::deserialize(&bytes).unwrap();
        println!("{:?}", parsed);
    }

    #[test]
    fn test_parse_feed() {
        let hex = "00000000000000000000000009000000000000000000000093b9ddc05000510002000000";
        let bytes = hex::decode(hex).unwrap();
        let parsed: TunnelFeed = bincode::deserialize(&bytes).unwrap();
        println!("{:?}", parsed);
    }
}
