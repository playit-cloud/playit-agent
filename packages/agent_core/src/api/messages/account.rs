use std::net::IpAddr;
use crate::api::messages::{ApiRequest, SimpleApiRequest};
use serde::{Serialize, Deserialize};
use uuid::Uuid;
use playit_agent_proto::PortProto;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum AccountApiRequest {
    #[serde(rename = "create-tunnel")]
    CreateTunnel(CreateTunnel),

    #[serde(rename = "list-account-tunnels")]
    ListAccountTunnels(ListAccountTunnels),
}

impl SimpleApiRequest for AccountApiRequest {
    type Response = AccountApiResponse;

    fn endpoint() -> &'static str {
        "/account"
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CreateTunnel {
    pub tunnel_type: Option<TunnelType>,
    pub name: Option<String>,
    pub port_type: PortProto,
    pub port_count: u16,
    pub local_ip: IpAddr,
    pub local_port: Option<u16>,
    pub agent_id: Option<Uuid>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ListAccountTunnels;

impl ApiRequest for ListAccountTunnels {
    type RequestJson = AccountApiRequest;
    type ResponseJson = AccountApiResponse;
    type Response = AccountTunnels;

    fn to_req(self) -> Self::RequestJson {
        AccountApiRequest::ListAccountTunnels(ListAccountTunnels)
    }

    fn extract_response(parsed: Self::ResponseJson) -> Option<Self::Response> {
        match parsed {
            AccountApiResponse::AccountTunnels(v) => Some(v),
            _ => None,
        }
    }

    fn endpoint() -> &'static str {
        "/account"
    }
}

impl ApiRequest for CreateTunnel {
    type RequestJson = AccountApiRequest;
    type ResponseJson = AccountApiResponse;
    type Response = Created;

    fn to_req(self) -> Self::RequestJson {
        AccountApiRequest::CreateTunnel(self)
    }

    fn extract_response(parsed: Self::ResponseJson) -> Option<Self::Response> {
        match parsed {
            AccountApiResponse::Created(v) => Some(v),
            _ => None,
        }
    }

    fn endpoint() -> &'static str {
        "/account"
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum AccountApiResponse {
    #[serde(rename = "created")]
    Created(Created),

    #[serde(rename = "account-tunnels")]
    AccountTunnels(AccountTunnels),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Created {
    pub id: Uuid,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AccountTunnels {
    pub tunnels: Vec<AccountTunnel>,
    pub agent_id: Option<Uuid>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AccountTunnel {
    pub id: Uuid,
    pub enabled: bool,
    pub name: Option<String>,
    pub ip_address: IpAddr,
    pub ip_hostname: String,
    pub custom_domain: Option<CustomDomain>,
    pub assigned_domain: String,
    pub display_address: String,
    pub is_dedicated_ip: bool,
    pub from_port: u16,
    pub to_port: u16,
    pub tunnel_type: Option<TunnelType>,
    pub port_type: PortProto,
    pub firewall_id: Option<Uuid>,
    pub protocol: TunnelProtocol,
}


#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
pub enum TunnelType {
    #[serde(rename = "minecraft-java")]
    MinecraftJava,
    #[serde(rename = "minecraft-bedrock")]
    MinecraftBedrock,
    #[serde(rename = "valheim")]
    Valheim,
    #[serde(rename = "terraria")]
    Terraria,
    #[serde(rename = "starbound")]
    Starbound,
    #[serde(rename = "rust")]
    Rust,
    #[serde(rename = "7days")]
    SevenDays,
    #[serde(rename = "unturned")]
    Unturned,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CustomDomain {
    pub id: Uuid,
    pub name: String,
    pub target: Option<CustomDomainTarget>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum CustomDomainTarget {
    #[serde(rename = "port-allocation")]
    PortAllocation { id: Uuid },
    #[serde(rename = "ip-address")]
    IpAddress { ip: IpAddr },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "protocol")]
pub enum TunnelProtocol {
    #[serde(rename = "to-agent")]
    ToAgent {
        local_ip: IpAddr,
        local_port: u16,
        agent_id: Option<u64>,
    }
}