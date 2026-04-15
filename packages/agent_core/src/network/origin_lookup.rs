use std::{
    borrow::Cow,
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};

use playit_agent_proto::PortProto;
use playit_api_client::api::{AgentRunDataV1, AgentTunnelV1, PortType, ProxyProtocol, TunnelType};
use tokio::net::lookup_host;
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
        let mut next = HashMap::new();

        for res in resources {
            match res.proto {
                PortProto::Tcp => {
                    next.insert(
                        Key {
                            tunnel_id: res.tunnel_id,
                            is_tcp: true,
                        },
                        res,
                    );
                }
                PortProto::Udp => {
                    next.insert(
                        Key {
                            tunnel_id: res.tunnel_id,
                            is_tcp: false,
                        },
                        res,
                    );
                }
                PortProto::Both => {
                    next.insert(
                        Key {
                            tunnel_id: res.tunnel_id,
                            is_tcp: true,
                        },
                        res.clone(),
                    );
                    next.insert(
                        Key {
                            tunnel_id: res.tunnel_id,
                            is_tcp: false,
                        },
                        res,
                    );
                }
            }
        }

        let mut lock = self.map.write().await;
        *lock = next;
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
pub enum OriginIp {
    IpAddress(IpAddr),
    Hostname(String),
}

impl OriginIp {
    pub fn display(&self) -> Cow<'_, str> {
        match self {
            OriginIp::IpAddress(ip) => Cow::Owned(ip.to_string()),
            OriginIp::Hostname(hostname) => Cow::Borrowed(hostname.as_str()),
        }
    }

    pub fn display_with_port(&self, port: u16) -> String {
        match self {
            OriginIp::IpAddress(ip) => SocketAddr::new(*ip, port).to_string(),
            OriginIp::Hostname(hostname) => format!("{hostname}:{port}"),
        }
    }

    async fn resolve(&self, port: u16) -> Option<SocketAddr> {
        match self {
            OriginIp::IpAddress(ip) => Some(SocketAddr::new(*ip, port)),
            OriginIp::Hostname(hostname) => {
                let mut addrs = match lookup_host((hostname.as_str(), port)).await {
                    Ok(addrs) => addrs,
                    Err(error) => {
                        tracing::error!(
                            ?error,
                            %hostname,
                            port,
                            "failed to resolve origin hostname"
                        );
                        return None;
                    }
                };

                addrs.next()
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum OriginTarget {
    Https {
        ip: OriginIp,
        http_port: u16,
        https_port: u16,
    },
    Port {
        ip: OriginIp,
        port: u16,
    },
}

impl OriginResource {
    fn parse_origin_ip(tunn: &AgentTunnelV1) -> OriginIp {
        tunn.agent_config
            .fields
            .iter()
            .find(|f| f.name.eq("local_ip"))
            .map(|v| v.value.trim())
            .filter(|v| !v.is_empty())
            .map(|value| {
                IpAddr::from_str(value)
                    .map(OriginIp::IpAddress)
                    .unwrap_or_else(|_| OriginIp::Hostname(value.to_owned()))
            })
            .unwrap_or_else(|| OriginIp::IpAddress("127.0.0.1".parse().unwrap()))
    }

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
                ip: Self::parse_origin_ip(tunn),
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
            _ => {
                // Get local_port from config, or fall back to public port from display_address
                let local_port = tunn
                    .agent_config
                    .fields
                    .iter()
                    .find(|f| f.name.eq("local_port"))
                    .and_then(|v| u16::from_str(&v.value).ok())
                    .or_else(|| {
                        // Extract port from display_address (format: "hostname:port" or "ip:port")
                        tunn.display_address
                            .rsplit(':')
                            .next()
                            .and_then(|p| u16::from_str(p).ok())
                    })?;

                OriginTarget::Port {
                    ip: Self::parse_origin_ip(tunn),
                    port: local_port,
                }
            }
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

    pub async fn resolve_local(&self, port_offset: u16) -> Option<SocketAddr> {
        match &self.target {
            OriginTarget::Https {
                ip,
                http_port,
                https_port,
            } => {
                if port_offset == 0 {
                    ip.resolve(*http_port).await
                } else if port_offset == 1 {
                    ip.resolve(*https_port).await
                } else {
                    None
                }
            }
            OriginTarget::Port { ip, port } => {
                if self.port_count == 0 {
                    return ip.resolve(*port).await;
                }

                if self.port_count <= port_offset {
                    return None;
                }

                let resolved_port = port.checked_add(port_offset)?;
                ip.resolve(resolved_port).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use playit_api_client::api::{AgentTunnelAttr, AgentTunnelConfig};
    use uuid::Uuid;

    fn build_tunnel(
        tunnel_type: Option<&str>,
        local_ip: &str,
        local_port: Option<&str>,
        port_type: PortType,
        port_count: u16,
    ) -> AgentTunnelV1 {
        let mut fields = vec![AgentTunnelAttr {
            name: "local_ip".to_owned(),
            value: local_ip.to_owned(),
        }];

        if let Some(local_port) = local_port {
            fields.push(AgentTunnelAttr {
                name: "local_port".to_owned(),
                value: local_port.to_owned(),
            });
        }

        AgentTunnelV1 {
            id: Uuid::nil(),
            internal_id: 7,
            name: "test".to_owned(),
            display_address: "public.example:25565".to_owned(),
            port_type,
            port_count,
            tunnel_type: tunnel_type.map(str::to_owned),
            tunnel_type_display: "test".to_owned(),
            agent_config: AgentTunnelConfig { fields },
            disabled_reason: None,
        }
    }

    #[test]
    fn from_agent_tunnel_preserves_hostname_target() {
        let tunnel = build_tunnel(None, "origin.internal", Some("25565"), PortType::Tcp, 0);

        let resource = OriginResource::from_agent_tunnel(&tunnel).expect("resource");

        match resource.target {
            OriginTarget::Port {
                ip: OriginIp::Hostname(hostname),
                port,
            } => {
                assert_eq!(hostname, "origin.internal");
                assert_eq!(port, 25565);
            }
            target => panic!("unexpected target: {target:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_local_supports_hostname_lookup() {
        let resource = OriginResource {
            tunnel_id: 1,
            proto: PortProto::Tcp,
            target: OriginTarget::Port {
                ip: OriginIp::Hostname("localhost".to_owned()),
                port: 8080,
            },
            port_count: 0,
            proxy_protocol: None,
        };

        let resolved = resource.resolve_local(0).await.expect("resolved");

        assert_eq!(resolved.port(), 8080);
        assert!(resolved.ip().is_loopback());
    }
}
