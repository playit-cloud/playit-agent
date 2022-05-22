use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ClaimProto, Proto};

pub const DEFAULT_API: &str = "https://api.playit.cloud/agent";

#[derive(Serialize, Deserialize, Debug, JsonSchema, PartialEq, Clone)]
pub struct AgentConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_address: Option<SocketAddr>,
    #[serde(default)]
    pub refresh_from_api: bool,
    pub secret_key: String,
    #[serde(alias = "mapping")]
    pub mappings: Vec<PortMappingConfig>,
}

impl AgentConfig {
    pub fn valid_secret_key(&self) -> bool {
        if self.secret_key.len() % 2 == 1 {
            return false;
        }

        for char in self.secret_key.chars() {
            if !char.is_ascii_hexdigit() {
                return false;
            }
        }

        true
    }

    pub fn get_api_url(&self) -> String {
        match &self.api_url {
            Some(v) => v.clone(),
            None => DEFAULT_API.to_string()
        }
    }
}

#[derive(Serialize, Deserialize, Debug, JsonSchema, PartialEq, Clone)]
pub struct PortMappingConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    pub proto: ClaimProto,
    pub tunnel_ip: IpAddr,
    #[serde(alias = "tunnel_port")]
    pub tunnel_from_port: u16,
    pub tunnel_to_port: Option<u16>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_ip: Option<IpAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_ip: Option<IpAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_port: Option<u16>,
}

impl AgentConfig {
    pub fn find_local_addr(
        &self,
        addr: SocketAddr,
        proto: Proto,
    ) -> Option<(Option<IpAddr>, SocketAddr)> {
        for mapping in &self.mappings {
            match (mapping.proto, proto) {
                (ClaimProto::Udp, Proto::Tcp) => continue,
                (ClaimProto::Tcp, Proto::Udp) => continue,
                _ => {}
            }

            if !mapping.tunnel_ip.eq(&addr.ip()) {
                continue;
            }

            let range = mapping.tunnel_from_port
                ..mapping
                .tunnel_to_port
                .unwrap_or(mapping.tunnel_from_port + 1);
            if !range.contains(&addr.port()) {
                continue;
            }

            let port_delta = addr.port() - mapping.tunnel_from_port;
            let local_port = mapping.local_port.unwrap_or(mapping.tunnel_from_port) + port_delta;

            let local_ip = mapping.local_ip.unwrap_or_else(|| Ipv4Addr::new(127, 0, 0, 1).into());
            return Some((
                mapping.bind_ip,
                SocketAddr::new(local_ip, local_port),
            ));
        }

        None
    }
}
