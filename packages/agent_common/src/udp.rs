use std::net::{Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use byteorder::{BigEndian, ByteOrder, ReadBytesExt, WriteBytesExt};

pub const REDIRECT_FLOW_4_FOOTER_ID_OLD: u64 = 0x5cb867cf788173b2;
pub const REDIRECT_FLOW_4_FOOTER_ID: u64 = 0x4448474f48414344;
pub const REDIRECT_FLOW_6_FOOTER_ID: u64 = 0x6668676f68616366;
pub const UDP_CHANNEL_ESTABLISH_ID: u64 = 0xd01fe6830ddce781;

const V4_LEN: usize = 20;
const V6_LEN: usize = 48;

#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Debug)]
pub enum RedirectFlowFooter {
    V4 {
        src: SocketAddrV4,
        dst: SocketAddrV4,
    },
    V6 {
        src: (Ipv6Addr, u16),
        dst: (Ipv6Addr, u16),
        flow: u32,
    },
}

impl RedirectFlowFooter {
    pub fn flip(self) -> Self {
        match self {
            RedirectFlowFooter::V4 { src, dst } => RedirectFlowFooter::V4 { src: dst, dst: src },
            RedirectFlowFooter::V6 { src, dst, flow } => RedirectFlowFooter::V6 { src: dst, dst: src, flow },
        }
    }

    pub fn src(&self) -> SocketAddr {
        match self {
            RedirectFlowFooter::V4 { src, .. } => SocketAddr::V4(*src),
            RedirectFlowFooter::V6 { src: (ip, port), flow, .. } => SocketAddr::V6(SocketAddrV6::new(*ip, *port, *flow, 0)),
        }
    }

    pub fn dst(&self) -> SocketAddr {
        match self {
            RedirectFlowFooter::V4 { dst, .. } => SocketAddr::V4(*dst),
            RedirectFlowFooter::V6 { dst: (ip, port), flow, .. } => SocketAddr::V6(SocketAddrV6::new(*ip, *port, *flow, 0)),
        }
    }

    pub fn write_to(&self, mut slice: &mut [u8]) -> bool {
        if slice.len() < self.len() {
            return false;
        }

        match self {
            RedirectFlowFooter::V4 { src, dst } => {
                slice.write_u32::<BigEndian>((*src.ip()).into()).unwrap();
                slice.write_u32::<BigEndian>((*dst.ip()).into()).unwrap();
                slice.write_u16::<BigEndian>(src.port()).unwrap();
                slice.write_u16::<BigEndian>(dst.port()).unwrap();
                slice.write_u64::<BigEndian>(REDIRECT_FLOW_4_FOOTER_ID_OLD).unwrap();
            }
            RedirectFlowFooter::V6 { src, dst, flow } => {
                slice.write_u128::<BigEndian>(src.0.into()).unwrap();
                slice.write_u128::<BigEndian>(dst.0.into()).unwrap();
                slice.write_u16::<BigEndian>(src.1).unwrap();
                slice.write_u16::<BigEndian>(dst.1).unwrap();
                slice.write_u32::<BigEndian>(*flow).unwrap();
                slice.write_u64::<BigEndian>(REDIRECT_FLOW_6_FOOTER_ID).unwrap();
            }
        }

        true
    }

    pub fn from_tail(mut slice: &[u8]) -> Option<RedirectFlowFooter> {
        /* not enough space for footer */
        if slice.len() < 8 {
            return None;
        }
        let footer = BigEndian::read_u64(&slice[slice.len() - 8..]);

        match footer {
            REDIRECT_FLOW_4_FOOTER_ID | REDIRECT_FLOW_4_FOOTER_ID_OLD => {
                if slice.len() < V4_LEN {
                    return None;
                }

                slice = &slice[slice.len() - V4_LEN..];

                let src_ip = slice.read_u32::<BigEndian>().unwrap();
                let dst_ip = slice.read_u32::<BigEndian>().unwrap();
                let src_port = slice.read_u16::<BigEndian>().unwrap();
                let dst_port = slice.read_u16::<BigEndian>().unwrap();

                Some(RedirectFlowFooter::V4 {
                    src: SocketAddrV4::new(src_ip.into(), src_port),
                    dst: SocketAddrV4::new(dst_ip.into(), dst_port),
                })
            }
            REDIRECT_FLOW_6_FOOTER_ID => {
                if slice.len() < V6_LEN {
                    return None;
                }

                slice = &slice[slice.len() - V6_LEN..];

                let src_ip = slice.read_u128::<BigEndian>().unwrap();
                let dst_ip = slice.read_u128::<BigEndian>().unwrap();
                let src_port = slice.read_u16::<BigEndian>().unwrap();
                let dst_port = slice.read_u16::<BigEndian>().unwrap();
                let flow = slice.read_u32::<BigEndian>().unwrap();

                Some(RedirectFlowFooter::V6 {
                    src: (src_ip.into(), src_port),
                    dst: (dst_ip.into(), dst_port),
                    flow,
                })
            }
            _ => None
        }
    }

    pub fn len(&self) -> usize {
        match self {
            RedirectFlowFooter::V4 { .. } => V4_LEN,
            RedirectFlowFooter::V6 { .. } => V6_LEN,
        }
    }

    pub const fn len_v4() -> usize {
        V4_LEN
    }

    pub const fn len_v6() -> usize {
        V6_LEN
    }
}

#[cfg(test)]
mod test {
    use crate::udp::{RedirectFlowFooter, V4_LEN, V6_LEN};

    #[test]
    fn test_serialize_v4() {
        let mut buf = [0u8; 100];
        let flow = RedirectFlowFooter::V4 {
            src: "123.234.13.43:8891".parse().unwrap(),
            dst: "123.99.13.43:773".parse().unwrap(),
        };
        flow.write_to(&mut buf[100 - V4_LEN..]);

        let parsed = RedirectFlowFooter::from_tail(&buf).unwrap();
        assert_eq!(flow, parsed);
    }

    #[test]
    fn test_serialize_v6() {
        let mut buf = [0u8; 100];
        let flow = RedirectFlowFooter::V6 {
            src: ("2602:fbaf::100".parse().unwrap(), 142),
            dst: ("2602:fbaf::200".parse().unwrap(), 142),
            flow: 1234,
        };
        flow.write_to(&mut buf[100 - V6_LEN..]);

        let parsed = RedirectFlowFooter::from_tail(&buf).unwrap();
        assert_eq!(flow, parsed);
    }

    #[test]
    fn test_parse() {
        let data = hex::decode("746573740a93b9ddc0c0a811b304d2ada35cb867cf788173b2").unwrap();
        let parsed = RedirectFlowFooter::from_tail(&data).unwrap();
        println!("Data: {:?}", parsed);
    }
}
