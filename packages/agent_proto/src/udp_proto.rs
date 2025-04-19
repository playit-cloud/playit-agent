use std::{net::{Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6}, num::{NonZeroU16, NonZeroU64}};

use byteorder::{BigEndian, ByteOrder, ReadBytesExt, WriteBytesExt};
use message_encoding::m_max_list;

pub const REDIRECT_FLOW_4_FOOTER_ID_V1: u64 = 0x5cb867cf788173b2;
pub const REDIRECT_FLOW_6_FOOTER_ID_V1: u64 = 0x6668676f68616366;

pub const REDIRECT_FLOW_4_FOOTER_ID_V2: u64 = 0x5cb867cf78817399;
pub const REDIRECT_FLOW_6_FOOTER_ID_V2: u64 = 0x6cb667cf78817369;

pub const UDP_CHANNEL_ESTABLISH_ID: u64 = 0xd01fe6830ddce781;

const EXT_LEN: usize = 18;

const IP4_LEN_V1: usize = 20;
const IP4_LEN_V2_WITHOUT_FRAG: usize = 20 + EXT_LEN /* extension */ + 2 /* packet id = 0 */;
const IP4_LEN_V2_WITH_FRAG: usize = IP4_LEN_V2_WITHOUT_FRAG + 3;

const IP6_LEN_V1: usize = 48;
const IP6_LEN_V2: usize = IP6_LEN_V1 - 4 /* remove flow */ + EXT_LEN /* client_server_id */;

#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Debug)]
pub enum UdpFlow {
    V4 {
        src: SocketAddrV4,
        dst: SocketAddrV4,
        frag: Option<FragmentInfo>,
        extension: Option<UdpFlowExtension>,
    },
    V6 {
        src: (Ipv6Addr, u16),
        dst: (Ipv6Addr, u16),
        extension: Option<UdpFlowExtension>,
    },
}

#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Debug)]
pub struct UdpFlowExtension {
    pub client_server_id: NonZeroU64,
    pub tunnel_id: NonZeroU64,
    pub port_offset: u16,
}

#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Debug)]
pub struct FragmentInfo {
    pub packet_id: NonZeroU16,
    pub frag_offset: u16,
    pub has_more: bool,
}

impl UdpFlow {
    pub fn client_server_id(&self) -> Option<NonZeroU64> {
        self.extension().map(|v| v.client_server_id)
    }

    pub fn extension(&self) -> Option<&UdpFlowExtension> {
        match self {
            Self::V4 { extension, .. } => extension.as_ref(),
            Self::V6 { extension, .. } => extension.as_ref(),
        }
    }

    pub fn flip(mut self) -> Self {
        match &mut self {
            UdpFlow::V4 { src, dst, .. } => {
                std::mem::swap(src, dst);
            },
            UdpFlow::V6 { src, dst, .. } => {
                std::mem::swap(src, dst);
            }
        };

        self
    }

    pub fn src(&self) -> SocketAddr {
        match self {
            UdpFlow::V4 { src, .. } => SocketAddr::V4(*src),
            UdpFlow::V6 {
                src: (ip, port),
                ..
            } => SocketAddr::V6(SocketAddrV6::new(*ip, *port, 0, 0)),
        }
    }

    pub fn dst(&self) -> SocketAddr {
        match self {
            UdpFlow::V4 { dst, .. } => SocketAddr::V4(*dst),
            UdpFlow::V6 {
                dst: (ip, port),
                ..
            } => SocketAddr::V6(SocketAddrV6::new(*ip, *port, 0, 0)),
        }
    }

    pub fn write_to(&self, mut slice: &mut [u8]) -> bool {
        if slice.len() < self.footer_len() {
            return false;
        }

        match self {
            UdpFlow::V4 { src, dst, frag, extension } => {
                slice.write_u32::<BigEndian>((*src.ip()).into()).unwrap();
                slice.write_u32::<BigEndian>((*dst.ip()).into()).unwrap();
                slice.write_u16::<BigEndian>(src.port()).unwrap();
                slice.write_u16::<BigEndian>(dst.port()).unwrap();

                if let Some(extension) = extension {
                    slice.write_u64::<BigEndian>(extension.client_server_id.get()).unwrap();
                    slice.write_u64::<BigEndian>(extension.tunnel_id.get()).unwrap();
                    slice.write_u16::<BigEndian>(extension.port_offset).unwrap();

                    match frag {
                        None => {
                            /* packet id = 0 */
                            slice.write_u16::<BigEndian>(0).unwrap()
                        }
                        Some(frag) => {
                            slice.write_u8(if frag.has_more { 1 } else { 0 }).unwrap();
                            slice.write_u16::<BigEndian>(frag.frag_offset).unwrap();
                            slice.write_u16::<BigEndian>(frag.packet_id.get()).unwrap();
                        }
                    }

                    slice.write_u64::<BigEndian>(REDIRECT_FLOW_4_FOOTER_ID_V2).unwrap();
                } else {
                    slice.write_u64::<BigEndian>(REDIRECT_FLOW_4_FOOTER_ID_V1).unwrap()
                }
            }
            UdpFlow::V6 { src, dst, extension } => {
                slice.write_u128::<BigEndian>(src.0.into()).unwrap();
                slice.write_u128::<BigEndian>(dst.0.into()).unwrap();
                slice.write_u16::<BigEndian>(src.1).unwrap();
                slice.write_u16::<BigEndian>(dst.1).unwrap();

                if let Some(extension) = extension {
                    slice.write_u64::<BigEndian>(extension.client_server_id.get()).unwrap();
                    slice.write_u64::<BigEndian>(extension.tunnel_id.get()).unwrap();
                    slice.write_u16::<BigEndian>(extension.port_offset).unwrap();
                    slice.write_u64::<BigEndian>(REDIRECT_FLOW_6_FOOTER_ID_V2).unwrap();
                } else {
                    /* flow label (no longer used) */
                    slice.write_u32::<BigEndian>(0).unwrap();
                    slice.write_u64::<BigEndian>(REDIRECT_FLOW_6_FOOTER_ID_V1) .unwrap();
                }
            }
        }

        true
    }

    pub fn from_tail(mut slice: &[u8]) -> Result<UdpFlow, Option<u64>> {
        /* not enough space for footer */
        if slice.len() < 8 {
            return Err(None);
        }

        let footer_id = BigEndian::read_u64(&slice[slice.len() - 8..]);
        println!("Parsed footer: {}", footer_id);

        match footer_id {
            REDIRECT_FLOW_4_FOOTER_ID_V1 => {
                if slice.len() < IP4_LEN_V1 {
                    return Err(None);
                }

                slice = &slice[slice.len() - IP4_LEN_V1..];

                let src_ip = slice.read_u32::<BigEndian>().unwrap();
                let dst_ip = slice.read_u32::<BigEndian>().unwrap();
                let src_port = slice.read_u16::<BigEndian>().unwrap();
                let dst_port = slice.read_u16::<BigEndian>().unwrap();

                Ok(UdpFlow::V4 {
                    src: SocketAddrV4::new(src_ip.into(), src_port),
                    dst: SocketAddrV4::new(dst_ip.into(), dst_port),
                    frag: None,
                    extension: None,
                })
            }
            REDIRECT_FLOW_4_FOOTER_ID_V2 => {
                if slice.len() < 10 {
                    return Err(None);
                }

                let packet_id = BigEndian::read_u16(&slice[slice.len() - 10..]);

                slice = if packet_id == 0 {
                    if slice.len() < IP4_LEN_V2_WITHOUT_FRAG {
                        return Err(None);
                    }
                    &slice[slice.len() - IP4_LEN_V2_WITHOUT_FRAG..]
                } else {
                    if slice.len() < IP4_LEN_V2_WITH_FRAG {
                        return Err(None);
                    }
                    &slice[slice.len() - IP4_LEN_V2_WITH_FRAG..]
                };

                let src_ip = slice.read_u32::<BigEndian>().unwrap();
                let dst_ip = slice.read_u32::<BigEndian>().unwrap();
                let src_port = slice.read_u16::<BigEndian>().unwrap();
                let dst_port = slice.read_u16::<BigEndian>().unwrap();
                let client_server_id = NonZeroU64::new(slice.read_u64::<BigEndian>().unwrap()).ok_or(None)?;
                let tunnel_id = NonZeroU64::new(slice.read_u64::<BigEndian>().unwrap()).ok_or(None)?;
                let port_offset = slice.read_u16::<BigEndian>().unwrap();

                let frag = if let Some(packet_id) = NonZeroU16::new(packet_id) {
                    let has_more = slice.read_u8().unwrap() != 0;
                    let frag_offset = slice.read_u16::<BigEndian>().unwrap();

                    Some(FragmentInfo {
                        packet_id,
                        frag_offset,
                        has_more,
                    })
                } else {
                    None
                };

                Ok(UdpFlow::V4 {
                    src: SocketAddrV4::new(src_ip.into(), src_port),
                    dst: SocketAddrV4::new(dst_ip.into(), dst_port),
                    frag,
                    extension: Some(UdpFlowExtension {
                        client_server_id,
                        tunnel_id,
                        port_offset,
                    }),
                })
            }
            REDIRECT_FLOW_6_FOOTER_ID_V1 => {
                if slice.len() < IP6_LEN_V1 {
                    return Err(None);
                }

                slice = &slice[slice.len() - IP6_LEN_V1..];

                let src_ip = slice.read_u128::<BigEndian>().unwrap();
                let dst_ip = slice.read_u128::<BigEndian>().unwrap();
                let src_port = slice.read_u16::<BigEndian>().unwrap();
                let dst_port = slice.read_u16::<BigEndian>().unwrap();
                let _flow = slice.read_u32::<BigEndian>().unwrap();

                Ok(UdpFlow::V6 {
                    src: (src_ip.into(), src_port),
                    dst: (dst_ip.into(), dst_port),
                    extension: None,
                })
            }
            REDIRECT_FLOW_6_FOOTER_ID_V2 => {
                if slice.len() < IP6_LEN_V2 {
                    return Err(None);
                }

                slice = &slice[slice.len() - IP6_LEN_V2..];

                let src_ip = slice.read_u128::<BigEndian>().unwrap();
                let dst_ip = slice.read_u128::<BigEndian>().unwrap();
                let src_port = slice.read_u16::<BigEndian>().unwrap();
                let dst_port = slice.read_u16::<BigEndian>().unwrap();

                let client_server_id = NonZeroU64::new(slice.read_u64::<BigEndian>().unwrap()).ok_or(None)?;
                let tunnel_id = NonZeroU64::new(slice.read_u64::<BigEndian>().unwrap()).ok_or(None)?;
                let port_offset = slice.read_u16::<BigEndian>().unwrap();

                Ok(UdpFlow::V6 {
                    src: (src_ip.into(), src_port),
                    dst: (dst_ip.into(), dst_port),
                    extension: Some(UdpFlowExtension {
                        client_server_id,
                        tunnel_id,
                        port_offset,
                    }),
                })
            }
            _ => Err(Some(footer_id)),
        }
    }

    pub fn footer_len(&self) -> usize {
        match self {
            UdpFlow::V4 { extension: None, .. } => IP4_LEN_V1,
            UdpFlow::V4 { extension: Some(_), frag: Some(_), .. } => IP4_LEN_V2_WITH_FRAG,
            UdpFlow::V4 { extension: Some(_), frag: None, .. } => IP4_LEN_V2_WITHOUT_FRAG,
            UdpFlow::V6 { extension: None, .. } => IP6_LEN_V1,
            UdpFlow::V6 { extension: Some(_), .. } => IP6_LEN_V2,
        }
    }

    pub const MAX_IP4_LEN: usize = {
        m_max_list(&[
            IP4_LEN_V1,
            IP4_LEN_V2_WITH_FRAG,
            IP4_LEN_V2_WITHOUT_FRAG,
        ])
    };

    pub const MAX_IP6_LEN: usize = {
        m_max_list(&[
            IP6_LEN_V1,
            IP6_LEN_V2,
        ])
    };

    pub const MX_LEN: usize = {
        m_max_list(&[
            Self::MAX_IP4_LEN,
            Self::MAX_IP6_LEN,
        ])
    };
}

#[cfg(test)]
mod test {
    use std::num::NonZeroU64;

    use super::{UdpFlow, UdpFlowExtension};

    #[test]
    fn udp_flow_v4_test() {
        let mut data = vec![0u8; 1024];
        let flow = UdpFlow::V4 {
            src: "4.2.1.3:1234".parse().unwrap(),
            dst: "1.2.3.4:5512".parse().unwrap(),
            frag: None,
            extension: Some(UdpFlowExtension {
                port_offset: 123,
                tunnel_id: NonZeroU64::new(123).unwrap(),
                client_server_id: NonZeroU64::new(12).unwrap(),
            }),
        };

        flow.write_to(&mut data[100..]);

        let parsed = UdpFlow::from_tail(&data[..100+flow.footer_len()]).unwrap();
        assert_eq!(flow, parsed);
    }

    #[test]
    fn udp_flow_v6_test() {
        let mut data = vec![0u8; 1024];
        let flow = UdpFlow::V6 {
            src: ("2601:1c2:c100:555:20f:53ff:fe4e:e541".parse().unwrap(), 100),
            dst: ("2601:1c2:c100:555:20f:53ff:fe4e:e541".parse().unwrap(), 999),
            extension: Some(UdpFlowExtension {
                port_offset: 999,
                tunnel_id: NonZeroU64::new(123).unwrap(),
                client_server_id: NonZeroU64::new(12).unwrap(),
            }),
        };

        flow.write_to(&mut data[100..]);

        let parsed = UdpFlow::from_tail(&data[..100+flow.footer_len()]).unwrap();
        assert_eq!(flow, parsed);
    }
}
