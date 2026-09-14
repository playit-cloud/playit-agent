//! Maps tunnel ids to the local services they forward to.

pub mod lan;
pub mod proxy_protocol;

use std::{
    collections::HashMap,
    fmt::Display,
    net::{IpAddr, SocketAddr},
    str::FromStr,
    sync::RwLock,
};

use playit_agent_proto::PortProto;
use playit_api_client::api::{AgentRunDataV1, AgentTunnelV1, PortType, ProxyProtocol, TunnelType};
use tokio::net::lookup_host;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    Tcp,
    Udp,
}

/// Thread-safe lookup table, replaced wholesale whenever the API run data changes.
#[derive(Default)]
pub struct OriginLookup {
    map: RwLock<HashMap<(u64, Transport), OriginResource>>,
}

impl OriginLookup {
    pub fn update_from_run_data(&self, run_data: &AgentRunDataV1) {
        self.update(
            run_data
                .tunnels
                .iter()
                .filter_map(OriginResource::from_agent_tunnel),
        );
    }

    pub fn update<I: IntoIterator<Item = OriginResource>>(&self, resources: I) {
        let mut next = HashMap::new();

        for resource in resources {
            let transports: &[Transport] = match resource.proto {
                PortProto::Tcp => &[Transport::Tcp],
                PortProto::Udp => &[Transport::Udp],
                PortProto::Both => &[Transport::Tcp, Transport::Udp],
            };
            for &transport in transports {
                next.insert((resource.tunnel_id, transport), resource.clone());
            }
        }

        *self.map.write().unwrap_or_else(|e| e.into_inner()) = next;
    }

    pub fn lookup(&self, tunnel_id: u64, transport: Transport) -> Option<OriginResource> {
        self.map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(tunnel_id, transport))
            .cloned()
    }
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

impl Display for OriginIp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OriginIp::IpAddress(ip) => write!(f, "{ip}"),
            OriginIp::Hostname(host) => write!(f, "{host}"),
        }
    }
}

impl OriginIp {
    async fn resolve(&self, port: u16) -> Option<SocketAddr> {
        match self {
            OriginIp::IpAddress(ip) => Some(SocketAddr::new(*ip, port)),
            OriginIp::Hostname(hostname) => match lookup_host((hostname.as_str(), port)).await {
                Ok(mut addrs) => addrs.next(),
                Err(error) => {
                    tracing::error!(
                        ?error,
                        %hostname,
                        port,
                        "failed to resolve configured local hostname for tunnel; check the tunnel local address or local DNS configuration"
                    );
                    None
                }
            },
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

fn config_field<'a>(tunnel: &'a AgentTunnelV1, name: &str) -> Option<&'a str> {
    tunnel
        .agent_config
        .fields
        .iter()
        .find(|field| field.name == name)
        .map(|field| field.value.trim())
        .filter(|value| !value.is_empty())
}

impl OriginResource {
    fn parse_origin_ip(tunnel: &AgentTunnelV1) -> OriginIp {
        match config_field(tunnel, "local_ip") {
            Some(value) => IpAddr::from_str(value)
                .map(OriginIp::IpAddress)
                .unwrap_or_else(|_| OriginIp::Hostname(value.to_owned())),
            None => OriginIp::IpAddress(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        }
    }

    pub fn from_agent_tunnel(tunnel: &AgentTunnelV1) -> Option<Self> {
        let tunnel_type = tunnel
            .tunnel_type
            .clone()
            .and_then(|v| serde_json::from_value::<TunnelType>(serde_json::Value::String(v)).ok());

        let proxy_protocol = config_field(tunnel, "proxy_protocol").and_then(|value| {
            serde_json::from_value::<ProxyProtocol>(serde_json::Value::String(value.to_owned()))
                .ok()
        });

        let target = match tunnel_type {
            Some(TunnelType::Https) => OriginTarget::Https {
                ip: Self::parse_origin_ip(tunnel),
                http_port: config_field(tunnel, "http_port")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(80),
                https_port: config_field(tunnel, "https_port")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(443),
            },
            _ => {
                let local_port = config_field(tunnel, "local_port")
                    .and_then(|v| v.parse().ok())
                    .or_else(|| {
                        tunnel
                            .display_address
                            .rsplit(':')
                            .next()
                            .and_then(|p| p.parse().ok())
                    })?;

                OriginTarget::Port {
                    ip: Self::parse_origin_ip(tunnel),
                    port: local_port,
                }
            }
        };

        Some(OriginResource {
            tunnel_id: tunnel.internal_id,
            proto: match tunnel.port_type {
                PortType::Tcp => PortProto::Tcp,
                PortType::Udp => PortProto::Udp,
                PortType::Both => PortProto::Both,
            },
            target,
            port_count: tunnel.port_count,
            proxy_protocol,
        })
    }

    /// Local address for the `port_offset`-th port of this tunnel.
    pub async fn resolve_local(&self, port_offset: u16) -> Option<SocketAddr> {
        match &self.target {
            OriginTarget::Https {
                ip,
                http_port,
                https_port,
            } => match port_offset {
                0 => ip.resolve(*http_port).await,
                1 => ip.resolve(*https_port).await,
                _ => None,
            },
            OriginTarget::Port { ip, port } => {
                if self.port_count == 0 {
                    return ip.resolve(*port).await;
                }
                if self.port_count <= port_offset {
                    return None;
                }
                ip.resolve(port.checked_add(port_offset)?).await
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

    #[test]
    fn falls_back_to_display_address_port() {
        let tunnel = build_tunnel(None, "", None, PortType::Both, 0);
        let resource = OriginResource::from_agent_tunnel(&tunnel).expect("resource");

        match resource.target {
            OriginTarget::Port {
                ip: OriginIp::IpAddress(ip),
                port,
            } => {
                assert!(ip.is_loopback());
                assert_eq!(port, 25565);
            }
            target => panic!("unexpected target: {target:?}"),
        }
    }

    #[test]
    fn both_proto_is_visible_for_tcp_and_udp() {
        let lookup = OriginLookup::default();
        let tunnel = build_tunnel(None, "127.0.0.1", Some("100"), PortType::Both, 0);
        lookup.update(OriginResource::from_agent_tunnel(&tunnel));

        assert!(lookup.lookup(7, Transport::Tcp).is_some());
        assert!(lookup.lookup(7, Transport::Udp).is_some());
        assert!(lookup.lookup(8, Transport::Udp).is_none());

        let tunnel = build_tunnel(None, "127.0.0.1", Some("100"), PortType::Udp, 0);
        lookup.update(OriginResource::from_agent_tunnel(&tunnel));
        assert!(lookup.lookup(7, Transport::Tcp).is_none());
        assert!(lookup.lookup(7, Transport::Udp).is_some());
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

    #[tokio::test]
    async fn resolve_local_applies_port_offsets_within_range() {
        let resource = OriginResource {
            tunnel_id: 1,
            proto: PortProto::Udp,
            target: OriginTarget::Port {
                ip: OriginIp::IpAddress(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
                port: 5000,
            },
            port_count: 3,
            proxy_protocol: None,
        };

        assert_eq!(resource.resolve_local(0).await.unwrap().port(), 5000);
        assert_eq!(resource.resolve_local(2).await.unwrap().port(), 5002);
        assert!(resource.resolve_local(3).await.is_none());
    }
}
