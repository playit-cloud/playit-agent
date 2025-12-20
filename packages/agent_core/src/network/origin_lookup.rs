use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};

use playit_agent_proto::PortProto;
use playit_api_client::api::{AgentRunDataV1, AgentTunnelV1, PortType, ProxyProtocol, TunnelType};
use tokio::sync::RwLock;

#[derive(Default)]
pub struct OriginLookup {
    map: RwLock<HashMap<Key, OriginResource>>,
}

impl OriginLookup {
    pub async fn update_from_run_data(&self, run_data: &AgentRunDataV1) {
        self.update(
            run_data
                .tunnels
                .iter()
                .filter_map(OriginResource::from_agent_tunnel),
        )
        .await;
    }

    pub async fn update<I: Iterator<Item = OriginResource>>(&self, resources: I) {
        let mut lock = self.map.write().await;
        lock.clear();

        for res in resources {
            match res.proto {
                PortProto::Tcp => {
                    lock.insert(
                        Key {
                            tunnel_id: res.tunnel_id,
                            is_tcp: true,
                        },
                        res,
                    );
                }
                PortProto::Udp => {
                    lock.insert(
                        Key {
                            tunnel_id: res.tunnel_id,
                            is_tcp: false,
                        },
                        res,
                    );
                }
                PortProto::Both => {
                    lock.insert(
                        Key {
                            tunnel_id: res.tunnel_id,
                            is_tcp: true,
                        },
                        res.clone(),
                    );
                    lock.insert(
                        Key {
                            tunnel_id: res.tunnel_id,
                            is_tcp: false,
                        },
                        res,
                    );
                }
            }
        }
    }

    pub async fn lookup(&self, tunnel_id: u64, is_tcp: bool) -> Option<OriginResource> {
        self.map
            .read()
            .await
            .get(&Key { tunnel_id, is_tcp })
            .cloned()
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct Key {
    tunnel_id: u64,
    is_tcp: bool,
}

#[derive(Debug, Clone)]
pub struct OriginResource {
    pub tunnel_id: u64,
    pub proto: PortProto,
    pub target: OriginTarget,
    pub port_count: u16,
    pub proxy_protocol: Option<ProxyProtocol>,
}

#[derive(Debug, Clone)]
pub enum OriginTarget {
    Https {
        ip: IpAddr,
        http_port: u16,
        https_port: u16,
    },
    Port {
        ip: IpAddr,
        port: u16,
    },
}

impl OriginResource {
    pub fn from_agent_tunnel(tunn: &AgentTunnelV1) -> Option<Self> {
        let tunnel_type = tunn
            .tunnel_type
            .clone()
            .and_then(|v| serde_json::from_value::<TunnelType>(serde_json::Value::String(v)).ok());

        let proxy_protocol = tunn
            .agent_config
            .fields
            .iter()
            .find(|f| f.name.eq("proxy_protocol"))
            .and_then(|v| {
                serde_json::from_value::<ProxyProtocol>(serde_json::Value::String(v.value.clone()))
                    .ok()
            });

        let target = match tunnel_type {
            Some(TunnelType::Https) => OriginTarget::Https {
                ip: tunn
                    .agent_config
                    .fields
                    .iter()
                    .find(|f| f.name.eq("local_ip"))
                    .and_then(|v| IpAddr::from_str(&v.value).ok())
                    .unwrap_or_else(|| "127.0.0.1".parse().unwrap()),
                http_port: tunn
                    .agent_config
                    .fields
                    .iter()
                    .find(|f| f.name.eq("http_port"))
                    .and_then(|v| u16::from_str(&v.value).ok())
                    .unwrap_or(80),
                https_port: tunn
                    .agent_config
                    .fields
                    .iter()
                    .find(|f| f.name.eq("https_port"))
                    .and_then(|v| u16::from_str(&v.value).ok())
                    .unwrap_or(443),
            },
            _ => OriginTarget::Port {
                ip: tunn
                    .agent_config
                    .fields
                    .iter()
                    .find(|f| f.name.eq("local_ip"))
                    .and_then(|v| IpAddr::from_str(&v.value).ok())
                    .unwrap_or_else(|| "127.0.0.1".parse().unwrap()),
                port: tunn
                    .agent_config
                    .fields
                    .iter()
                    .find(|f| f.name.eq("local_port"))
                    .and_then(|v| u16::from_str(&v.value).ok())?,
            },
        };

        Some(OriginResource {
            tunnel_id: tunn.internal_id,
            proto: match tunn.port_type {
                PortType::Tcp => PortProto::Tcp,
                PortType::Udp => PortProto::Udp,
                PortType::Both => PortProto::Both,
            },
            target,
            port_count: tunn.port_count,
            proxy_protocol,
        })
    }

    pub fn resolve_local(&self, port_offset: u16) -> Option<SocketAddr> {
        match &self.target {
            OriginTarget::Https {
                ip,
                http_port,
                https_port,
            } => {
                if port_offset == 0 {
                    Some(SocketAddr::new(*ip, *http_port))
                } else if port_offset == 1 {
                    Some(SocketAddr::new(*ip, *https_port))
                } else {
                    None
                }
            }
            OriginTarget::Port { ip, port } => {
                if self.port_count == 0 {
                    return Some(SocketAddr::new(*ip, *port));
                }

                if self.port_count <= port_offset {
                    return None;
                }

                Some(SocketAddr::new(*ip, *port + port_offset))
            }
        }
    }
}
