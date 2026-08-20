use std::collections::HashSet;
use std::net::IpAddr;
use std::num::{NonZeroU16, NonZeroU64};

use crate::problem::{Problem, ProblemCode, ProblemSubject, SubjectKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TunnelId(NonZeroU64);

impl TunnelId {
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelProtocol {
    Tcp,
    Udp,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyProtocol {
    None,
    V1,
    V2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginHost {
    Ip(IpAddr),
    Hostname(Hostname),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hostname(String);

impl Hostname {
    pub fn parse(value: impl Into<String>) -> Result<Self, TunnelValidationError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 253
            && !value.starts_with('.')
            && !value.ends_with('.')
            && value.split('.').all(valid_label);
        valid
            .then_some(Self(value))
            .ok_or(TunnelValidationError::InvalidHostname)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginTarget {
    Port {
        host: OriginHost,
        first_port: NonZeroU16,
    },
    Https {
        host: OriginHost,
        http_port: NonZeroU16,
        https_port: NonZeroU16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelSpec {
    pub id: TunnelId,
    pub protocol: TunnelProtocol,
    pub target: OriginTarget,
    pub port_count: NonZeroU16,
    pub proxy_protocol: ProxyProtocol,
}

impl TunnelSpec {
    pub fn target_port(&self, offset: u16) -> Option<NonZeroU16> {
        if offset >= self.port_count.get() {
            return None;
        }

        match &self.target {
            OriginTarget::Port { first_port, .. } => first_port
                .get()
                .checked_add(offset)
                .and_then(NonZeroU16::new),
            OriginTarget::Https {
                http_port,
                https_port,
                ..
            } => match offset {
                0 => Some(*http_port),
                1 => Some(*https_port),
                _ => None,
            },
        }
    }

    pub fn validate(&self) -> Result<(), TunnelValidationError> {
        if matches!(self.target, OriginTarget::Https { .. }) && self.port_count.get() != 2 {
            return Err(TunnelValidationError::HttpsPortCount);
        }

        if let OriginTarget::Port { first_port, .. } = &self.target {
            let last_offset = self.port_count.get() - 1;
            if first_port.get().checked_add(last_offset).is_none() {
                return Err(TunnelValidationError::PortRangeOverflow);
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelAvailability {
    Active,
    Disabled(Problem),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTunnel {
    pub spec: TunnelSpec,
    pub availability: TunnelAvailability,
    pub public_address: String,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TunnelCatalog {
    revision: u64,
    accepted_at_millis: Option<u64>,
    tunnels: Vec<CatalogTunnel>,
}

impl TunnelCatalog {
    pub fn try_new(
        revision: u64,
        accepted_at_millis: u64,
        tunnels: Vec<CatalogTunnel>,
    ) -> Result<Self, CatalogValidationError> {
        let mut ids = HashSet::with_capacity(tunnels.len());
        for tunnel in &tunnels {
            tunnel
                .spec
                .validate()
                .map_err(|error| CatalogValidationError::InvalidTunnel {
                    id: tunnel.spec.id,
                    error,
                })?;
            if !ids.insert(tunnel.spec.id) {
                return Err(CatalogValidationError::DuplicateTunnel(tunnel.spec.id));
            }
        }

        Ok(Self {
            revision,
            accepted_at_millis: Some(accepted_at_millis),
            tunnels,
        })
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub const fn accepted_at_millis(&self) -> Option<u64> {
        self.accepted_at_millis
    }

    pub fn tunnels(&self) -> &[CatalogTunnel] {
        &self.tunnels
    }

    pub fn problem_for(error: &CatalogValidationError) -> Problem {
        let subject = match error {
            CatalogValidationError::DuplicateTunnel(id)
            | CatalogValidationError::InvalidTunnel { id, .. } => Some(ProblemSubject {
                kind: SubjectKind::Tunnel,
                id: id.get(),
            }),
        };
        let mut problem = Problem::new(ProblemCode::CatalogInvalid);
        problem.subject = subject;
        problem
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelValidationError {
    InvalidHostname,
    PortRangeOverflow,
    HttpsPortCount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogValidationError {
    DuplicateTunnel(TunnelId),
    InvalidTunnel {
        id: TunnelId,
        error: TunnelValidationError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u64) -> TunnelId {
        TunnelId::new(NonZeroU64::new(value).unwrap())
    }

    fn tunnel(value: u64, first_port: u16, count: u16) -> CatalogTunnel {
        CatalogTunnel {
            spec: TunnelSpec {
                id: id(value),
                protocol: TunnelProtocol::Tcp,
                target: OriginTarget::Port {
                    host: OriginHost::Ip("127.0.0.1".parse().unwrap()),
                    first_port: NonZeroU16::new(first_port).unwrap(),
                },
                port_count: NonZeroU16::new(count).unwrap(),
                proxy_protocol: ProxyProtocol::None,
            },
            availability: TunnelAvailability::Active,
            public_address: format!("demo-{value}.playit.gg:{first_port}"),
            disabled_reason: None,
        }
    }

    #[test]
    fn hostnames_are_validated_without_network_access() {
        assert_eq!(
            Hostname::parse("origin.internal").unwrap().as_str(),
            "origin.internal"
        );
        assert_eq!(
            Hostname::parse("bad host"),
            Err(TunnelValidationError::InvalidHostname)
        );
        assert_eq!(
            Hostname::parse("-bad.example"),
            Err(TunnelValidationError::InvalidHostname)
        );
    }

    #[test]
    fn port_ranges_are_checked_before_publication() {
        let error = TunnelCatalog::try_new(1, 10, vec![tunnel(1, u16::MAX, 2)]).unwrap_err();
        assert_eq!(
            error,
            CatalogValidationError::InvalidTunnel {
                id: id(1),
                error: TunnelValidationError::PortRangeOverflow,
            }
        );
    }

    #[test]
    fn catalogs_reject_duplicate_ids_atomically() {
        let error =
            TunnelCatalog::try_new(1, 10, vec![tunnel(7, 80, 1), tunnel(7, 81, 1)]).unwrap_err();
        assert_eq!(error, CatalogValidationError::DuplicateTunnel(id(7)));
    }

    #[test]
    fn target_port_respects_normalized_count() {
        let tunnel = tunnel(1, 4000, 2);
        assert_eq!(tunnel.spec.target_port(0).unwrap().get(), 4000);
        assert_eq!(tunnel.spec.target_port(1).unwrap().get(), 4001);
        assert_eq!(tunnel.spec.target_port(2), None);
    }

    #[test]
    fn https_tunnels_require_both_routing_ports() {
        let tunnel = CatalogTunnel {
            spec: TunnelSpec {
                id: id(1),
                protocol: TunnelProtocol::Tcp,
                target: OriginTarget::Https {
                    host: OriginHost::Ip("127.0.0.1".parse().unwrap()),
                    http_port: NonZeroU16::new(80).unwrap(),
                    https_port: NonZeroU16::new(443).unwrap(),
                },
                port_count: NonZeroU16::MIN,
                proxy_protocol: ProxyProtocol::None,
            },
            availability: TunnelAvailability::Active,
            public_address: "demo.playit.gg".to_owned(),
            disabled_reason: None,
        };
        assert_eq!(
            TunnelCatalog::try_new(1, 10, vec![tunnel]),
            Err(CatalogValidationError::InvalidTunnel {
                id: id(1),
                error: TunnelValidationError::HttpsPortCount,
            })
        );
    }
}
