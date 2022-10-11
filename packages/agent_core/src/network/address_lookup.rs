use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::Arc;

use playit_agent_proto::PortProto;

pub trait AddressLookup: 'static {
    fn find_tunnel_port_range(&self, match_ip: Ipv6Addr, port: u16, proto: PortProto) -> Option<(u16, u16)>;

    fn local_address(&self, match_addr: MatchAddress, proto: PortProto) -> Option<SocketAddr>;

    fn local_mapping(&self, tunnel_addr: SocketAddr, proto: PortProto) -> Option<SocketAddr> {
        let match_addr = self.tunnel_match_address(tunnel_addr, proto)?;
        let mut local_addr = self.local_address(match_addr, proto)?;

        let port_offset = tunnel_addr.port() - match_addr.from_port;
        let local_port = local_addr.port();
        local_addr.set_port(local_port + port_offset);

        Some(local_addr)
    }

    fn tunnel_match_address(&self, tunnel_addr: SocketAddr, proto: PortProto) -> Option<MatchAddress> {
        let (match_ip, port) = match tunnel_addr {
            SocketAddr::V6(addr) => (Self::match_ip_v6(*addr.ip()), addr.port()),
            SocketAddr::V4(addr) => (Self::match_ip_v4(*addr.ip()), addr.port()),
        };

        let (from_port, to_port) = self.find_tunnel_port_range(match_ip, port, proto)?;
        Some(MatchAddress {
            ip: match_ip,
            from_port,
            to_port,
        })
    }

    fn match_ip(ip: IpAddr) -> Ipv6Addr {
        match ip {
            IpAddr::V4(ip) => Self::match_ip_v4(ip),
            IpAddr::V6(ip) => Self::match_ip_v6(ip),
        }
    }

    fn match_ip_v4(ip: Ipv4Addr) -> Ipv6Addr {
        let mut octs = [0u8; 16];
        octs[15] = ip.octets()[3];
        octs.into()
    }

    fn match_ip_v6(ip: Ipv6Addr) -> Ipv6Addr {
        let mut seg = ip.segments();

        if seg[0] != 0x2602 || seg[1] != 0xfbaf || (seg[2] & 0xF000) != 0x0 {
            return ip;
        }

        seg[0] = 0;
        seg[1] = 0;
        seg[2] = 0;

        seg.into()
    }
}

impl<T: AddressLookup> AddressLookup for Arc<T> {
    fn find_tunnel_port_range(&self, match_ip: Ipv6Addr, port: u16, proto: PortProto) -> Option<(u16, u16)> {
        (&*self as &T).find_tunnel_port_range(match_ip, port, proto)
    }

    fn local_address(&self, match_addr: MatchAddress, proto: PortProto) -> Option<SocketAddr> {
        (&*self as &T).local_address(match_addr, proto)
    }

    fn tunnel_match_address(&self, tunnel_addr: SocketAddr, proto: PortProto) -> Option<MatchAddress> {
        (&*self as &T).tunnel_match_address(tunnel_addr, proto)
    }

    fn match_ip(ip: IpAddr) -> Ipv6Addr {
        T::match_ip(ip)
    }

    fn match_ip_v4(ip: Ipv4Addr) -> Ipv6Addr {
        T::match_ip_v4(ip)
    }

    fn match_ip_v6(ip: Ipv6Addr) -> Ipv6Addr {
        T::match_ip_v6(ip)
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct MatchAddress {
    pub ip: Ipv6Addr,
    pub from_port: u16,
    pub to_port: u16,
}
