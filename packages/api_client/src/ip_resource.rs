use byteorder::{BigEndian, ByteOrder};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash)]
pub struct IpResource {
    pub ip_num: u64,
    pub region: PlayitRegion,
}

impl IpResource {
    pub fn from_ip(ip: IpAddr) -> Self {
        let (region, ip_num) = PlayitRegion::from_ip(ip);

        let is_region_ip = ip_num >= 64 && ip_num < 128;
        let region = match (is_region_ip, region) {
            (true, Some(region)) => region,
            _ => PlayitRegion::Anycast,
        };

        IpResource { ip_num, region }
    }

    pub fn to_tunnel_ip(&self) -> Ipv6Addr {
        self.region.tunnel_address(self.ip_num)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum PlayitRegion {
    Anycast = 0,
    Global = 1,
    NorthAmerica = 2,
    Europe = 3,
    Asia = 4,
    India = 5,
    SouthAmerica = 6,
}

impl PlayitRegion {
    pub fn from_ip(ip: IpAddr) -> (Option<Self>, u64) {
        match ip {
            IpAddr::V4(ip) => Self::from_ip4(ip),
            IpAddr::V6(ip) => Self::from_ip6(ip),
        }
    }

    pub fn from_ip4(ip: Ipv4Addr) -> (Option<Self>, u64) {
        let octs = ip.octets();

        let net = match (octs[0], octs[1], octs[2]) {
            (0, 0, 0) => Some(PlayitRegion::Anycast),
            (209, 25, 140) => Some(PlayitRegion::NorthAmerica),
            (209, 25, 141) => Some(PlayitRegion::Europe),
            (209, 25, 142) => Some(PlayitRegion::Asia),
            (209, 25, 143) => Some(PlayitRegion::India),
            (23, 133, 216) => Some(PlayitRegion::SouthAmerica),
            (198, 22, 204) => Some(PlayitRegion::SouthAmerica),
            (147, 185, 221) => Some(PlayitRegion::Global),
            _ => None,
        };

        (net, octs[3] as _)
    }

    pub fn from_ip6(ip: Ipv6Addr) -> (Option<Self>, u64) {
        let parts = ip.octets();

        let region_number = BigEndian::read_u16(&parts[6..8]);
        let ip_number = BigEndian::read_u64(&parts[8..]);

        let region = match region_number {
            0 => Some(PlayitRegion::Anycast),
            1 => Some(PlayitRegion::Global),
            2 => Some(PlayitRegion::NorthAmerica),
            3 => Some(PlayitRegion::Europe),
            4 => Some(PlayitRegion::Asia),
            5 => Some(PlayitRegion::India),
            6 => Some(PlayitRegion::SouthAmerica),
            _ => None,
        };

        (region, ip_number)
    }

    pub fn tunnel_address(&self, ip_number: u64) -> Ipv6Addr {
        let mut octs = [0u8; 16];
        octs[0] = 0x26;
        octs[1] = 0x02;
        octs[2] = 0xfb;
        octs[3] = 0xaf;
        BigEndian::write_u16(&mut octs[6..8], *self as u16);
        BigEndian::write_u64(&mut octs[8..], ip_number);
        octs.into()
    }
}
