use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::ops::Sub;

#[cfg(feature = "use-schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;

use crate::auth::SignatureError;

pub mod api;
pub mod auth;
pub mod rpc;
pub mod udp;
pub mod agent_config;
pub mod hmac;
pub mod utils;

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
pub struct RpcMessage<T> {
    pub request_id: u64,
    pub content: T,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
pub enum TunnelFeed {
    Response(RpcMessage<TunnelResponse>),
    NewClientV4(NewClientV4),
    NewClientV6(NewClientV6),
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
pub struct NewClientV4 {
    pub connect_addr: SocketAddrV4,
    pub peer_addr: SocketAddrV4,
    pub claim_instructions: ClaimInstructionVersioned,
    pub from_tunnel_server: String,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
pub struct NewClientV6 {
    pub connect_addr: SocketAddrV6,
    pub peer_addr: SocketAddrV6,
    pub claim_instructions: ClaimInstructionVersioned,
    pub from_tunnel_server: String,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
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

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
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
            Self::Tcp4 { address, token } => ClaimInstruction { address: address.into(), token },
            Self::Tcp6 { address, token } => ClaimInstruction { address: address.into(), token },
        }
    }
}

pub struct ClaimInstruction {
    pub address: SocketAddr,
    pub token: Vec<u8>,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Clone, Serialize, Deserialize, Debug)]
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

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
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

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
pub struct SetupUdpChannelDetailsV4 {
    pub tunnel_addr: SocketAddrV4,
    pub token: Vec<u8>,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
pub struct SetupUdpChannelDetailsV6 {
    pub tunnel_addr: SocketAddrV6,
    pub token: Vec<u8>,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
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

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
pub enum ClaimError {
    NotRegistered,
    LeaseConflict,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ClaimLeaseV4 {
    pub ip: Ipv4Addr,
    pub from_port: u16,
    pub to_port: u16,
    pub proto: ClaimProto,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Clone, Debug, Hash, PartialEq)]
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

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Hash, Debug, Clone, Copy, Eq, PartialEq)]
pub enum ClaimProto {
    #[serde(rename = "udp")]
    Udp,
    #[serde(rename = "tcp")]
    Tcp,
    #[serde(rename = "both")]
    Both,
}

impl ClaimProto {
    pub fn has_tcp(&self) -> bool {
        match self {
            ClaimProto::Udp => false,
            ClaimProto::Tcp => true,
            ClaimProto::Both => true,
        }
    }

    pub fn has_udp(&self) -> bool {
        match self {
            ClaimProto::Udp => true,
            ClaimProto::Tcp => false,
            ClaimProto::Both => true,
        }
    }
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
pub enum Proto {
    #[serde(rename = "udp")]
    Udp,
    #[serde(rename = "tcp")]
    Tcp,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentRegistered {
    pub account_id: u64,
    pub session_id: u64,
    pub session_timestamp: u64,
    pub signature: [u8; 32],
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Ping {
    pub id: u64,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
pub struct Pong {
    pub id: u64,
    pub tunnel_server_id: u64,
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug)]
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

#[cfg(test)]
mod test {
    use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6};
    use crate::{ClaimInstructionVersioned, TunnelFeed, TunnelRequest};

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

    #[test]
    fn parse() {
        let bytes = hex::decode("0100000093b9ddc0dd63c0a811b35cc7010000002602fbaf000000000000000000000002071c4c0000000000000004002993b9ddc063ddc0a811b3c75c83ed2e0d1c0762c756ad00000000000000010000000000000001dfd0220b1f16c7b3f337269d5b766584608f17c477b8c08f784cacd038c9370a000000040000000000000074657374").unwrap();
        let parsed: TunnelFeed = bincode::deserialize(&bytes).unwrap();
        match parsed {
            TunnelFeed::NewClientV4(client) => {
                match client.claim_instructions {
                    ClaimInstructionVersioned::Tcp4 { .. } => panic!(),
                    ClaimInstructionVersioned::Tcp6 { token, .. } => {
                        println!("{}", hex::encode(token));
                    }
                }
            }
            _ => panic!(),
        }
    }
}