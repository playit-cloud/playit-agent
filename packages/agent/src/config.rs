use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use messages::{ClaimProto, Proto};

pub const DEFAULT_API: &str = "https://api.playit.cloud/agent";

pub async fn load_or_create() -> std::io::Result<Option<AgentConfig>> {
    match tokio::fs::File::open("./playit.toml").await {
        Ok(mut file) => {
            let mut data = Vec::new();
            file.read_to_end(&mut data).await?;

            let config = match toml::from_slice::<AgentConfig>(&data) {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to parse ./playit.toml");
                    return Ok(None);
                }
            };

            Ok(Some(config))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut file = tokio::fs::File::create("./playit.toml").await?;

            file.write_all(
                toml::to_string(&AgentConfig {
                    api_url: None,
                    refresh_from_api: true,
                    secret_key: "put-secret-here".to_string(),
                    mappings: vec![],
                }).unwrap().as_bytes(),
            ).await?;

            Ok(None)
        }
        Err(e) => Err(e),
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AgentConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(default)]
    pub refresh_from_api: bool,
    pub secret_key: String,
    #[serde(alias = "mapping")]
    pub mappings: Vec<PortMapping>,
}

impl AgentConfig {
    pub fn valid_secret_key(&self) -> bool {
        hex::decode(&self.secret_key).is_ok()
    }

    pub fn get_api_url(&self) -> String {
        match &self.api_url {
            Some(v) => v.clone(),
            None => DEFAULT_API.to_string()
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PortMapping {
    pub proto: ClaimProto,

    pub tunnel_ip: Ipv4Addr,
    #[serde(alias = "tunnel_port")]
    pub tunnel_from_port: u16,
    pub tunnel_to_port: Option<u16>,

    pub bind_ip: Option<IpAddr>,

    pub local_ip: Option<Ipv4Addr>,
    pub local_port: Option<u16>,
}

impl AgentConfig {
    pub fn find_local_addr(
        &self,
        addr: SocketAddrV4,
        proto: Proto,
    ) -> Option<(Option<IpAddr>, SocketAddr)> {
        for mapping in &self.mappings {
            match (mapping.proto, proto) {
                (ClaimProto::Udp, Proto::Tcp) => continue,
                (ClaimProto::Tcp, Proto::Udp) => continue,
                _ => {}
            }

            if !mapping.tunnel_ip.eq(addr.ip()) {
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

            let local_ip = mapping.local_ip.unwrap_or_else(|| Ipv4Addr::new(127, 0, 0, 1));
            return Some((
                mapping.bind_ip,
                SocketAddr::V4(SocketAddrV4::new(local_ip, local_port)),
            ));
        }

        None
    }
}
