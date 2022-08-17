use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::{ClaimProto, Proto};

#[cfg(feature = "use-schema")]
use schemars::JsonSchema;

pub const DEFAULT_API: &'static str = "https://api.playit.cloud/agent";
pub const DEFAULT_CONTROL: &'static str = "control.playit.gg";

#[derive(Eq, PartialEq)]
pub struct AgentConfig {
    pub last_update: u64,
    pub api_url: String,
    pub ping_target_addresses: Vec<String>,
    pub control_address: String,
    pub api_refresh_rate: Option<u64>,
    pub ping_interval: u64,
    pub secret_key: String,
    pub mappings: Vec<PortMappingConfig>,

    pub api_url_set: bool,
    pub control_addr_set: bool,
}

impl AgentConfig {
    pub fn to_updated(self, update: AgentConfig) -> Self {
        match self.get_updated(update) {
            Some(v) => v,
            None => self
        }
    }

    pub fn get_updated(&self, mut updated: Self) -> Option<Self> {
        if !updated.api_url_set {
            updated.api_url = self.api_url.clone();
            updated.api_url_set = self.api_url_set;
        }

        if !updated.control_addr_set {
            updated.control_address = self.control_address.clone();
            updated.control_addr_set = self.control_addr_set;
        }

        if updated.eq(self) {
            return None;
        }

        Some(updated)
    }

    pub fn as_builder(&self) -> AgentConfigBuilder {
        AgentConfigBuilder {
            last_update: Some(self.last_update),
            api_url: Some(self.api_url.clone()),
            ping_targets: None,
            ping_target_addresses: Some(self.ping_target_addresses.clone()),
            control_address: Some(self.control_address.clone()),
            refresh_from_api: Some(self.api_refresh_rate.is_some()),
            api_refresh_rate: self.api_refresh_rate,
            ping_interval: Some(self.ping_interval),
            secret_key: self.secret_key.clone(),
            mappings: self.mappings.iter().map(|v| v.as_builder()).collect(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfigBuilder::default().build()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PortMappingConfig {
    pub name: Option<String>,
    pub preview_address: String,
    pub proto: ClaimProto,
    pub tunnel_ip: IpAddr,
    pub tunnel_from_port: u16,
    pub tunnel_to_port: u16,
    pub bind_ip: Option<IpAddr>,
    pub local_ip: IpAddr,
    pub local_port: u16,
}

impl PortMappingConfig {
    pub fn as_builder(&self) -> PortMappingBuilder {
        PortMappingBuilder {
            name: self.name.clone(),
            generated_address: None,
            custom_address: None,
            proto: self.proto,
            tunnel_ip: self.tunnel_ip,
            tunnel_from_port: self.tunnel_from_port,
            tunnel_to_port: if self.tunnel_to_port == self.tunnel_from_port + 1 { None } else { Some(self.tunnel_to_port) },
            bind_ip: self.bind_ip,
            local_ip: if self.local_ip == Ipv4Addr::new(127, 0, 0, 1) { None } else { Some(self.local_ip) },
            local_port: if self.local_port == self.tunnel_from_port { None } else { Some(self.local_port) },
        }
    }
}

/* user friendly versions with optionals */

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
pub struct AgentConfigBuilder {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ping_targets: Option<Vec<SocketAddr>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ping_target_addresses: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_from_api: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_refresh_rate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ping_interval: Option<u64>,
    pub secret_key: String,
    #[serde(alias = "mapping")]
    pub mappings: Vec<PortMappingBuilder>,
}

impl AgentConfigBuilder {
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

    pub fn build(self) -> AgentConfig {
        let mut ping_target_addresses = vec![];

        if let Some(targets) = self.ping_targets {
            for addr in targets {
                ping_target_addresses.push(addr.to_string());
            }
        }

        if let Some(targets) = self.ping_target_addresses {
            for addr in targets {
                ping_target_addresses.push(addr);
            }
        }

        let api_url_set = self.api_url.is_some();
        let control_addr_set = self.control_address.is_some();

        AgentConfig {
            last_update: self.last_update.unwrap_or(0),
            api_url: match self.api_url {
                Some(v) => v,
                None => DEFAULT_API.to_string()
            },
            ping_target_addresses,
            control_address: match self.control_address {
                Some(v) => v,
                None => DEFAULT_CONTROL.to_string(),
            },
            secret_key: self.secret_key,
            mappings: self.mappings.into_iter().map(|v| v.build()).collect(),
            api_refresh_rate: match (self.refresh_from_api, self.api_refresh_rate) {
                (Some(false), _) => None,
                (Some(true), None) | (None, None) => Some(5_000),
                (None, Some(0)) => None,
                (_, Some(rate)) => Some(rate.max(5_000)),
            },
            api_url_set,
            control_addr_set,
            ping_interval: self.ping_interval.unwrap_or(5_000),
        }
    }
}

#[cfg_attr(feature = "use-schema", derive(JsonSchema))]
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct PortMappingBuilder {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_address: Option<String>,

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

impl PortMappingBuilder {
    pub fn build(self) -> PortMappingConfig {
        PortMappingConfig {
            name: self.name,
            preview_address: match self.custom_address {
                Some(v) => v,
                None => match self.generated_address {
                    Some(v) => v,
                    None => SocketAddr::new(self.tunnel_ip, self.tunnel_from_port).to_string(),
                },
            },
            proto: self.proto,
            tunnel_ip: self.tunnel_ip,
            tunnel_from_port: self.tunnel_from_port,
            tunnel_to_port: self.tunnel_to_port.unwrap_or(self.tunnel_from_port + 1),
            bind_ip: self.bind_ip,
            local_ip: match self.local_ip {
                Some(v) => v,
                None => IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
            },
            local_port: match self.local_port {
                Some(port) => port,
                None => self.tunnel_from_port,
            },
        }
    }
}

impl AgentConfig {
    pub fn find_local_addr(
        &self,
        addr: SocketAddr,
        proto: Proto,
    ) -> Option<(Option<IpAddr>, SocketAddr)> {
        /* tunnel ip can be announced at multiple subnets for routing optimizations */
        let addr_ip_number = get_match_ip(addr.ip());

        for mapping in &self.mappings {
            match (mapping.proto, proto) {
                (ClaimProto::Udp, Proto::Tcp) => continue,
                (ClaimProto::Tcp, Proto::Udp) => continue,
                _ => {}
            }

            let range = mapping.tunnel_from_port..mapping.tunnel_to_port;

            if !range.contains(&addr.port()) {
                continue;
            }

            let port_delta = addr.port() - mapping.tunnel_from_port;
            let local_port = mapping.local_port + port_delta;

            let local_ip = mapping.local_ip;
            let found = (
                mapping.bind_ip,
                SocketAddr::new(local_ip, local_port),
            );

            let tunnel_ip_number = get_match_ip(mapping.tunnel_ip);
            if tunnel_ip_number == addr_ip_number {
                return Some(found);
            }
        }

        None
    }
}

pub fn get_match_ip(ip: IpAddr) -> Ipv6Addr {
    match ip {
        IpAddr::V4(ip) => {
            let mut octs = [0u8; 16];
            octs[15] = ip.octets()[3];
            octs.into()
        }
        IpAddr::V6(ip) => {
            let mut s = ip.segments();

            if s[0] != 0x2602 || s[1] != 0xfbaf || (s[2] & 0xF000) != 0x0 {
                return ip;
            }

            s[0] = 0;
            s[1] = 0;
            s[2] = 0;

            s.into()
        }
    }
}

#[cfg(test)]
mod test {
    use crate::agent_config::{AgentConfig, PortMappingConfig};
    use crate::{ClaimProto, Proto};

    #[test]
    fn test_find_local() {
        let mut agent_config = AgentConfig::default();

        agent_config.mappings.push(PortMappingConfig {
            name: None,
            preview_address: "".to_string(),
            proto: ClaimProto::Udp,
            tunnel_ip: "2602:fbaf:1c0::c0".parse().unwrap(),
            tunnel_from_port: 100,
            tunnel_to_port: 200,
            bind_ip: None,
            local_ip: "127.0.0.133".parse().unwrap(),
            local_port: 9912,
        });

        agent_config.mappings.push(PortMappingConfig {
            name: None,
            preview_address: "".to_string(),
            proto: ClaimProto::Udp,
            tunnel_ip: "32.12.41.2".parse().unwrap(),
            tunnel_from_port: 100,
            tunnel_to_port: 200,
            bind_ip: None,
            local_ip: "127.0.0.199".parse().unwrap(),
            local_port: 444,
        });

        assert_eq!(agent_config.find_local_addr(
            "[2602:fbaf:1c0::c0]:100".parse().unwrap(),
            Proto::Tcp,
        ), None);

        assert_eq!(agent_config.find_local_addr(
            "[2602:fbaf:1c0::c0]:100".parse().unwrap(),
            Proto::Udp,
        ).unwrap().1, "127.0.0.133:9912".parse().unwrap());

        assert_eq!(agent_config.find_local_addr(
            "[2602:fbaf:1c0::c0]:100".parse().unwrap(),
            Proto::Udp,
        ).unwrap().1, "127.0.0.133:9912".parse().unwrap());

        assert_eq!(agent_config.find_local_addr(
            "[2602:fbaf::c0]:100".parse().unwrap(),
            Proto::Udp,
        ).unwrap().1, "127.0.0.133:9912".parse().unwrap());

        assert_eq!(agent_config.find_local_addr(
            "[2603:fbaf:1c0::c0]:100".parse().unwrap(),
            Proto::Udp,
        ), None);

        assert_eq!(agent_config.find_local_addr(
            "[2602:fbaf::2]:100".parse().unwrap(),
            Proto::Udp,
        ).unwrap().1, "127.0.0.199:444".parse().unwrap());

        assert_eq!(agent_config.find_local_addr(
            "199.33.123.2:100".parse().unwrap(),
            Proto::Udp,
        ).unwrap().1, "127.0.0.199:444".parse().unwrap());
    }
}