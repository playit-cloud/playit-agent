use std::{
    collections::HashMap,
    fmt::Display,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};

use playit_agent_proto::PortProto;
use playit_api_client::api::{AgentRunDataV1, AgentTunnelV1, PortType, ProxyProtocol, TunnelType};
use tokio::net::lookup_host;
use tokio::sync::RwLock;

use crate::tls::CertManager;
use crate::tls::manager::certificate_domain;
use crate::tls::proxy::Upstream;

/// Value of the `tls_termination` field that makes the agent terminate TLS with a
/// Let's Encrypt certificate.
pub const TLS_TERMINATION_LETS_ENCRYPT: &str = "letsencrypt";

#[derive(Default)]
pub struct OriginLookup {
    map: RwLock<HashMap<Key, OriginResource>>,
    certs: Option<CertManager>,
}

impl OriginLookup {
    /// A lookup that keeps `certs` requesting certificates for the tunnels that terminate TLS.
    pub fn with_cert_manager(certs: CertManager) -> Self {
        OriginLookup {
            map: RwLock::default(),
            certs: Some(certs),
        }
    }

    pub fn cert_manager(&self) -> Option<&CertManager> {
        self.certs.as_ref()
    }

    pub async fn update_from_run_data(&self, run_data: &AgentRunDataV1) {
        // A disabled tunnel can't answer challenges, so no certificate is requested for it.
        let disabled: std::collections::HashSet<u64> = run_data
            .tunnels
            .iter()
            .filter(|tunnel| tunnel.disabled_reason.is_some())
            .map(|tunnel| tunnel.internal_id)
            .collect();
        self.update_inner(
            run_data
                .tunnels
                .iter()
                .filter_map(OriginResource::from_agent_tunnel),
            |res| !disabled.contains(&res.tunnel_id),
        )
        .await;
    }

    pub async fn update<I: Iterator<Item = OriginResource>>(&self, resources: I) {
        self.update_inner(resources, |_| true).await;
    }

    async fn update_inner<I, F>(&self, resources: I, wants_certificate: F)
    where
        I: Iterator<Item = OriginResource>,
        F: Fn(&OriginResource) -> bool,
    {
        let mut next = HashMap::new();
        let mut cert_domains = Vec::new();

        for res in resources {
            if let OriginTarget::HttpsTerminated {
                domain: Some(domain),
                ..
            } = &res.target
                && wants_certificate(&res)
            {
                cert_domains.push(domain.clone());
            }

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

        if let Some(certs) = &self.certs {
            certs.set_domains(cert_domains);
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
    /// An HTTPS tunnel whose TLS the agent terminates with a Let's Encrypt certificate.
    /// Decrypted requests go to the HTTP port, or to the HTTPS port when the HTTP port is 0.
    HttpsTerminated {
        ip: OriginIp,
        /// The domain the certificate is for; `None` when the tunnel's address isn't a
        /// domain Let's Encrypt can issue for.
        domain: Option<String>,
        http_port: u16,
        https_port: u16,
    },
    Port {
        ip: OriginIp,
        port: u16,
    },
}

impl OriginTarget {
    /// Picks the local server for a TLS-terminated tunnel. A port of 0 is disabled; when
    /// both are set, plain HTTP is used.
    pub fn terminated_upstream_port(http_port: u16, https_port: u16) -> Option<(bool, u16)> {
        match (http_port, https_port) {
            (0, 0) => None,
            (0, https) => Some((true, https)),
            (http, _) => Some((false, http)),
        }
    }

    /// Resolves the local server that a TLS-terminated tunnel forwards requests to.
    pub async fn resolve_terminated_upstream(&self) -> Option<Upstream> {
        let OriginTarget::HttpsTerminated {
            ip,
            http_port,
            https_port,
            ..
        } = self
        else {
            return None;
        };
        let (tls, port) = Self::terminated_upstream_port(*http_port, *https_port)?;
        let addr = ip.resolve(port).await?;
        Some(if tls {
            Upstream::Https(addr)
        } else {
            Upstream::Http(addr)
        })
    }
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

        let field = |name: &str| {
            tunn.agent_config
                .fields
                .iter()
                .find(|f| f.name.eq(name))
                .map(|f| f.value.trim())
        };

        let target = match &tunnel_type {
            Some(TunnelType::Https) => {
                let ip = Self::parse_origin_ip(tunn);
                let http_port = field("http_port")
                    .and_then(|v| u16::from_str(v).ok())
                    .unwrap_or(80);
                let https_port = field("https_port")
                    .and_then(|v| u16::from_str(v).ok())
                    .unwrap_or(443);

                if field("tls_termination")
                    .is_some_and(|v| v.eq_ignore_ascii_case(TLS_TERMINATION_LETS_ENCRYPT))
                {
                    OriginTarget::HttpsTerminated {
                        ip,
                        domain: certificate_domain(&tunn.display_address),
                        http_port,
                        https_port,
                    }
                } else {
                    OriginTarget::Https {
                        ip,
                        http_port,
                        https_port,
                    }
                }
            }
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
            proto: match (tunnel_type, tunn.port_type) {
                (Some(TunnelType::NetherNet), _) => PortProto::Udp,
                (_, PortType::Tcp) => PortProto::Tcp,
                (_, PortType::Udp) => PortProto::Udp,
                (_, PortType::Both) => PortProto::Both,
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
                // A port of 0 turns that half of the tunnel off.
                match port_offset {
                    0 if *http_port != 0 => ip.resolve(*http_port).await,
                    1 if *https_port != 0 => ip.resolve(*https_port).await,
                    _ => None,
                }
            }
            OriginTarget::HttpsTerminated { .. } => {
                match self.target.resolve_terminated_upstream().await? {
                    Upstream::Http(addr) | Upstream::Https(addr) => Some(addr),
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

    #[tokio::test]
    async fn nethernet_run_data_selects_udp_without_changing_generic_tunnels() {
        let lookup = OriginLookup::default();
        for (kind, expected_tcp) in [(Some("nethernet"), false), (None, true)] {
            let tunnel = build_tunnel(kind, "127.0.0.1", Some("19132"), PortType::Both, 1);
            lookup
                .update(std::iter::once(
                    OriginResource::from_agent_tunnel(&tunnel).unwrap(),
                ))
                .await;
            assert_eq!(
                lookup.lookup(tunnel.internal_id, true).await.is_some(),
                expected_tcp
            );
            let resource = lookup.lookup(tunnel.internal_id, false).await.unwrap();
            assert_eq!(
                resource.resolve_local(0).await.unwrap(),
                "127.0.0.1:19132".parse().unwrap()
            );
            assert!(resource.resolve_local(1).await.is_none());
        }
    }

    fn https_tunnel(fields: &[(&str, &str)]) -> AgentTunnelV1 {
        let mut tunnel = build_tunnel(Some("https"), "127.0.0.1", None, PortType::Tcp, 2);
        tunnel.display_address = "Site.Example.com".to_owned();
        tunnel
            .agent_config
            .fields
            .extend(fields.iter().map(|(name, value)| AgentTunnelAttr {
                name: (*name).to_owned(),
                value: (*value).to_owned(),
            }));
        tunnel
    }

    #[test]
    fn terminated_upstream_prefers_http_and_treats_zero_as_disabled() {
        assert_eq!(
            OriginTarget::terminated_upstream_port(80, 443),
            Some((false, 80))
        );
        assert_eq!(
            OriginTarget::terminated_upstream_port(8080, 0),
            Some((false, 8080))
        );
        assert_eq!(
            OriginTarget::terminated_upstream_port(0, 8443),
            Some((true, 8443))
        );
        assert_eq!(OriginTarget::terminated_upstream_port(0, 0), None);
    }

    #[tokio::test]
    async fn tls_termination_field_selects_terminated_target() {
        let tunnel = https_tunnel(&[
            ("tls_termination", "letsencrypt"),
            ("http_port", "0"),
            ("https_port", "8443"),
        ]);
        let resource = OriginResource::from_agent_tunnel(&tunnel).unwrap();
        match &resource.target {
            OriginTarget::HttpsTerminated {
                domain,
                http_port,
                https_port,
                ..
            } => {
                assert_eq!(domain.as_deref(), Some("site.example.com"));
                assert_eq!((*http_port, *https_port), (0, 8443));
            }
            target => panic!("unexpected target: {target:?}"),
        }
        assert_eq!(
            resource.target.resolve_terminated_upstream().await,
            Some(Upstream::Https("127.0.0.1:8443".parse().unwrap()))
        );

        let plain = OriginResource::from_agent_tunnel(&https_tunnel(&[])).unwrap();
        assert!(matches!(plain.target, OriginTarget::Https { .. }));
    }

    #[tokio::test]
    async fn passthrough_https_port_zero_disables_that_offset() {
        let tunnel = https_tunnel(&[("http_port", "0"), ("https_port", "8443")]);
        let resource = OriginResource::from_agent_tunnel(&tunnel).unwrap();
        assert!(resource.resolve_local(0).await.is_none());
        assert_eq!(
            resource.resolve_local(1).await,
            Some("127.0.0.1:8443".parse().unwrap())
        );
    }

    #[tokio::test]
    async fn lookup_registers_certificate_domains() {
        let certs = CertManager::new(crate::tls::CertStore::new(
            std::env::temp_dir().join("playit-origin-lookup-unused"),
        ));
        let lookup = OriginLookup::with_cert_manager(certs.clone());
        let tunnel = https_tunnel(&[("tls_termination", "letsencrypt")]);
        lookup
            .update(std::iter::once(
                OriginResource::from_agent_tunnel(&tunnel).unwrap(),
            ))
            .await;
        assert!(certs.status("site.example.com").is_some());

        lookup.update(std::iter::empty()).await;
        assert!(certs.status("site.example.com").is_none());

        let mut disabled = tunnel.clone();
        disabled.disabled_reason = Some("over tunnel limit".into());
        lookup
            .update_from_run_data(&AgentRunDataV1 {
                agent_id: Uuid::nil(),
                tunnels: vec![disabled],
                pending: vec![],
                notices: vec![],
                permissions: playit_api_client::api::AgentPermissions {
                    is_self_managed: true,
                    has_premium: false,
                    account_status: playit_api_client::api::AccountStatus::Verified,
                },
            })
            .await;
        assert!(certs.status("site.example.com").is_none());
    }
}
