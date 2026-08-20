use std::{
    collections::HashMap,
    fmt::Display,
    net::{IpAddr, SocketAddr},
};

use crate::gateway::{GatewayOrigin, GatewayOriginHost, GatewayOriginTarget};
use playit_agent_proto::PortProto;
use playit_model::{ProxyProtocol, TunnelProtocol};
use tokio::net::lookup_host;
use tokio::sync::RwLock;

#[derive(Default)]
pub struct OriginLookup {
    map: RwLock<HashMap<Key, OriginResource>>,
}

impl OriginLookup {
    pub async fn update_from_gateway(&self, origins: &[GatewayOrigin]) {
        self.update(origins.iter().cloned().map(OriginResource::from_gateway))
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
            OriginIp::Hostname(hostname) => {
                let mut addrs = match lookup_host((hostname.as_str(), port)).await {
                    Ok(addrs) => addrs,
                    Err(error) => {
                        tracing::error!(
                            ?error,
                            %hostname,
                            port,
                            "failed to resolve configured local hostname for tunnel; check the tunnel local address or local DNS configuration"
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
    pub fn from_gateway(origin: GatewayOrigin) -> Self {
        Self {
            tunnel_id: origin.tunnel_id,
            proto: match origin.protocol {
                TunnelProtocol::Tcp => PortProto::Tcp,
                TunnelProtocol::Udp => PortProto::Udp,
                TunnelProtocol::Both => PortProto::Both,
            },
            target: match origin.target {
                GatewayOriginTarget::Https {
                    host,
                    http_port,
                    https_port,
                } => OriginTarget::Https {
                    ip: origin_ip(host),
                    http_port,
                    https_port,
                },
                GatewayOriginTarget::Port { host, port } => OriginTarget::Port {
                    ip: origin_ip(host),
                    port,
                },
            },
            port_count: origin.port_count,
            proxy_protocol: match origin.proxy_protocol {
                ProxyProtocol::None => None,
                configured => Some(configured),
            },
        }
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

fn origin_ip(host: GatewayOriginHost) -> OriginIp {
    match host {
        GatewayOriginHost::Ip(address) => OriginIp::IpAddress(address),
        GatewayOriginHost::Hostname(hostname) => OriginIp::Hostname(hostname),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_origin_preserves_hostname_target() {
        let resource = OriginResource::from_gateway(GatewayOrigin {
            tunnel_id: 7,
            protocol: TunnelProtocol::Tcp,
            target: GatewayOriginTarget::Port {
                host: GatewayOriginHost::Hostname("origin.internal".to_owned()),
                port: 25565,
            },
            port_count: 0,
            proxy_protocol: ProxyProtocol::None,
        });

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
