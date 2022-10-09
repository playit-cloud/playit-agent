use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use playit_agent_proto::PortProto;

pub trait AddressLookup {
    fn tunnel_match_address(&self, tunnel_addr: SocketAddr) -> Option<MatchAddress> {
        let (match_ip, port) = match tunnel_addr {
            SocketAddr::V6(addr) => (Self::match_ip_v6(*addr.ip()), addr.port()),
            SocketAddr::V4(addr) => (Self::match_ip_v4(*addr.ip()), addr.port()),
        };

        let (from_port, to_port) = self.find_tunnel_port_range(match_ip, port)?;
        Some(MatchAddress {
            ip: match_ip,
            from_port,
            to_port
        })
    }

    fn find_tunnel_port_range(&self, match_ip: Ipv6Addr, port: u16) -> Option<(u16, u16)>;

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

    fn local_address(&self, match_addr: MatchAddress, proto: PortProto) -> Option<SocketAddr>;
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct MatchAddress {
    pub ip: Ipv6Addr,
    pub from_port: u16,
    pub to_port: u16,
}
