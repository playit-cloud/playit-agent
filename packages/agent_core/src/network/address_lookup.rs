use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use playit_agent_proto::PortProto;

pub trait AddressLookup {
    fn tunnel_match_address(&self, tunnel_addr: SocketAddr) -> Option<MatchAddress> {
        let (match_ip, port) = match tunnel_addr {
            SocketAddr::V6(addr) => (Self::match_ip_v6(*addr.ip()), addr.port()),
            SocketAddr::V4(addr) => (Self::match_ip_v4(*addr.ip()), addr.port()),
        };

        let start_port = self.find_tunnel_start_port(match_ip, port)?;
        Some(MatchAddress {
            ip: match_ip,
            port: start_port
        })
    }

    fn find_tunnel_start_port(&self, match_ip: Ipv6Addr, port: u16) -> Option<u16>;

    fn match_ip(ip: IpAddr) -> Ipv6Addr {
        match ip {
            IpAddr::V4(ip) => Self::match_ip_v4(ip),
            IpAddr::V6(ip) => Self::match_ip_v6(ip),
        }
    }

    fn match_ip_v4(ip: Ipv4Addr) -> Ipv6Addr {
        todo!()
    }

    fn match_ip_v6(ip: Ipv6Addr) -> Ipv6Addr {
        todo!()
    }

    fn local_address(&self, match_addr: MatchAddress, proto: PortProto) -> Option<SocketAddr>;
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debugn)]
pub struct MatchAddress {
    pub ip: Ipv6Addr,
    pub port: u16,
}
