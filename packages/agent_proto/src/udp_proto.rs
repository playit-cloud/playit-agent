use std::{
    net::{Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    num::{NonZeroU16, NonZeroU64},
};

use byteorder::{BigEndian, ByteOrder, ReadBytesExt, WriteBytesExt};
use message_encoding::m_max_list;

pub const REDIRECT_FLOW_4_FOOTER_ID_V1: u64 = 0x5cb867cf788173b2;
pub const REDIRECT_FLOW_6_FOOTER_ID_V1: u64 = 0x6668676f68616366;

pub const REDIRECT_FLOW_4_FOOTER_ID_V2: u64 = 0x5cb867cf78817399;
pub const REDIRECT_FLOW_6_FOOTER_ID_V2: u64 = 0x6cb667cf78817369;

/* From here on the footer id is the V2 magic with its low byte holding
 * FooterFlags: each flag appends its fields, in flag order, after the V2
 * extension. Readers reject flags they do not know. */
pub const REDIRECT_FLOW_4_FOOTER_BASE: u64 = REDIRECT_FLOW_4_FOOTER_ID_V2 & !FooterFlags::MASK;
pub const REDIRECT_FLOW_6_FOOTER_BASE: u64 = REDIRECT_FLOW_6_FOOTER_ID_V2 & !FooterFlags::MASK;

pub struct FooterFlags;

impl FooterFlags {
    pub const MASK: u64 = 0xff;
    /// A `u16` target port follows the extension.
    pub const HAS_TARGET_PORT: u64 = 1 << 0;
    pub const KNOWN: u64 = Self::HAS_TARGET_PORT;
}

pub const UDP_CHANNEL_ESTABLISH_ID: u64 = 0xd01fe6830ddce781;

const EXT_LEN: usize = 18;

const IP4_LEN_V1: usize = 20;
const IP4_LEN_V2_WITHOUT_FRAG: usize = 20 + EXT_LEN /* extension */ + 2 /* packet id = 0 */;
const IP4_LEN_V2_WITH_FRAG: usize = IP4_LEN_V2_WITHOUT_FRAG + 3;
const TARGET_PORT_LEN: usize = 2;

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
    /// Local port the agent must deliver this flow to, chosen per flow rather
    /// than per tunnel. Zero means none: deliver to the configured port plus
    /// `port_offset`.
    pub target_port: u16,
}

impl UdpFlowExtension {
    /// Flags for the fields this extension carries beyond V2; zero means a plain V2 footer.
    fn footer_flags(&self) -> u64 {
        if self.target_port == 0 {
            0
        } else {
            FooterFlags::HAS_TARGET_PORT
        }
    }

    /// Bytes the flagged fields add after the V2 extension.
    fn flagged_len(&self) -> usize {
        if self.target_port == 0 {
            0
        } else {
            TARGET_PORT_LEN
        }
    }
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

    pub fn update_client_server_id(&mut self, client_server_id: NonZeroU64) {
        if let Some(extension) = self.extension_mut() {
            extension.client_server_id = client_server_id;
        }
    }

    pub fn extension_mut(&mut self) -> Option<&mut UdpFlowExtension> {
        match self {
            Self::V4 { extension, .. } => extension.as_mut(),
            Self::V6 { extension, .. } => extension.as_mut(),
        }
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
            }
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
                src: (ip, port), ..
            } => SocketAddr::V6(SocketAddrV6::new(*ip, *port, 0, 0)),
        }
    }

    pub fn dst(&self) -> SocketAddr {
        match self {
            UdpFlow::V4 { dst, .. } => SocketAddr::V4(*dst),
            UdpFlow::V6 {
                dst: (ip, port), ..
            } => SocketAddr::V6(SocketAddrV6::new(*ip, *port, 0, 0)),
        }
    }

    pub fn write_to(&self, mut slice: &mut [u8]) -> bool {
        if slice.len() < self.footer_len() {
            return false;
        }

        match self {
            UdpFlow::V4 {
                src,
                dst,
                frag,
                extension,
            } => {
                slice.write_u32::<BigEndian>((*src.ip()).into()).unwrap();
                slice.write_u32::<BigEndian>((*dst.ip()).into()).unwrap();
                slice.write_u16::<BigEndian>(src.port()).unwrap();
                slice.write_u16::<BigEndian>(dst.port()).unwrap();

                if let Some(extension) = extension {
                    slice
                        .write_u64::<BigEndian>(extension.client_server_id.get())
                        .unwrap();
                    slice
                        .write_u64::<BigEndian>(extension.tunnel_id.get())
                        .unwrap();
                    slice.write_u16::<BigEndian>(extension.port_offset).unwrap();
                    if extension.footer_flags() & FooterFlags::HAS_TARGET_PORT != 0 {
                        slice.write_u16::<BigEndian>(extension.target_port).unwrap();
                    }

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

                    let footer_id = match extension.footer_flags() {
                        0 => REDIRECT_FLOW_4_FOOTER_ID_V2,
                        flags => REDIRECT_FLOW_4_FOOTER_BASE | flags,
                    };
                    slice.write_u64::<BigEndian>(footer_id).unwrap();
                } else {
                    slice
                        .write_u64::<BigEndian>(REDIRECT_FLOW_4_FOOTER_ID_V1)
                        .unwrap()
                }
            }
            UdpFlow::V6 {
                src,
                dst,
                extension,
            } => {
                slice.write_u128::<BigEndian>(src.0.into()).unwrap();
                slice.write_u128::<BigEndian>(dst.0.into()).unwrap();
                slice.write_u16::<BigEndian>(src.1).unwrap();
                slice.write_u16::<BigEndian>(dst.1).unwrap();

                if let Some(extension) = extension {
                    slice
                        .write_u64::<BigEndian>(extension.client_server_id.get())
                        .unwrap();
                    slice
                        .write_u64::<BigEndian>(extension.tunnel_id.get())
                        .unwrap();
                    slice.write_u16::<BigEndian>(extension.port_offset).unwrap();
                    if extension.footer_flags() & FooterFlags::HAS_TARGET_PORT != 0 {
                        slice.write_u16::<BigEndian>(extension.target_port).unwrap();
                    }
                    let footer_id = match extension.footer_flags() {
                        0 => REDIRECT_FLOW_6_FOOTER_ID_V2,
                        flags => REDIRECT_FLOW_6_FOOTER_BASE | flags,
                    };
                    slice.write_u64::<BigEndian>(footer_id).unwrap();
                } else {
                    /* flow label (no longer used) */
                    slice.write_u32::<BigEndian>(0).unwrap();
                    slice
                        .write_u64::<BigEndian>(REDIRECT_FLOW_6_FOOTER_ID_V1)
                        .unwrap();
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

        /* V1 and V2 ids are exact; anything else with a V2 magic carries FooterFlags */
        let (flagged_4, flagged_6, flags) = match footer_id {
            REDIRECT_FLOW_4_FOOTER_ID_V1
            | REDIRECT_FLOW_4_FOOTER_ID_V2
            | REDIRECT_FLOW_6_FOOTER_ID_V1
            | REDIRECT_FLOW_6_FOOTER_ID_V2 => (false, false, 0),
            id if id & !FooterFlags::MASK == REDIRECT_FLOW_4_FOOTER_BASE => {
                (true, false, id & FooterFlags::MASK)
            }
            id if id & !FooterFlags::MASK == REDIRECT_FLOW_6_FOOTER_BASE => {
                (false, true, id & FooterFlags::MASK)
            }
            _ => (false, false, 0),
        };
        if flags & !FooterFlags::KNOWN != 0 {
            return Err(Some(footer_id));
        }
        let with_target = flags & FooterFlags::HAS_TARGET_PORT != 0;
        let flagged_len = if with_target { TARGET_PORT_LEN } else { 0 };

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
            id if id == REDIRECT_FLOW_4_FOOTER_ID_V2 || flagged_4 => {
                if slice.len() < 10 {
                    return Err(None);
                }

                let packet_id = BigEndian::read_u16(&slice[slice.len() - 10..]);

                let len = if packet_id == 0 {
                    IP4_LEN_V2_WITHOUT_FRAG
                } else {
                    IP4_LEN_V2_WITH_FRAG
                } + flagged_len;
                if slice.len() < len {
                    return Err(None);
                }
                slice = &slice[slice.len() - len..];

                let src_ip = slice.read_u32::<BigEndian>().unwrap();
                let dst_ip = slice.read_u32::<BigEndian>().unwrap();
                let src_port = slice.read_u16::<BigEndian>().unwrap();
                let dst_port = slice.read_u16::<BigEndian>().unwrap();
                let client_server_id =
                    NonZeroU64::new(slice.read_u64::<BigEndian>().unwrap()).ok_or(None)?;
                let tunnel_id =
                    NonZeroU64::new(slice.read_u64::<BigEndian>().unwrap()).ok_or(None)?;
                let port_offset = slice.read_u16::<BigEndian>().unwrap();
                let target_port = if with_target {
                    slice.read_u16::<BigEndian>().unwrap()
                } else {
                    0
                };

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
                        target_port,
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
            id if id == REDIRECT_FLOW_6_FOOTER_ID_V2 || flagged_6 => {
                let len = IP6_LEN_V2 + flagged_len;
                if slice.len() < len {
                    return Err(None);
                }

                slice = &slice[slice.len() - len..];

                let src_ip = slice.read_u128::<BigEndian>().unwrap();
                let dst_ip = slice.read_u128::<BigEndian>().unwrap();
                let src_port = slice.read_u16::<BigEndian>().unwrap();
                let dst_port = slice.read_u16::<BigEndian>().unwrap();

                let client_server_id =
                    NonZeroU64::new(slice.read_u64::<BigEndian>().unwrap()).ok_or(None)?;
                let tunnel_id =
                    NonZeroU64::new(slice.read_u64::<BigEndian>().unwrap()).ok_or(None)?;
                let port_offset = slice.read_u16::<BigEndian>().unwrap();
                let target_port = if with_target {
                    slice.read_u16::<BigEndian>().unwrap()
                } else {
                    0
                };

                Ok(UdpFlow::V6 {
                    src: (src_ip.into(), src_port),
                    dst: (dst_ip.into(), dst_port),
                    extension: Some(UdpFlowExtension {
                        client_server_id,
                        tunnel_id,
                        port_offset,
                        target_port,
                    }),
                })
            }
            _ => Err(Some(footer_id)),
        }
    }

    pub fn footer_len(&self) -> usize {
        match self {
            UdpFlow::V4 {
                extension: None, ..
            } => IP4_LEN_V1,
            UdpFlow::V4 {
                extension: Some(ext),
                frag: Some(_),
                ..
            } => IP4_LEN_V2_WITH_FRAG + ext.flagged_len(),
            UdpFlow::V4 {
                extension: Some(ext),
                frag: None,
                ..
            } => IP4_LEN_V2_WITHOUT_FRAG + ext.flagged_len(),
            UdpFlow::V6 {
                extension: None, ..
            } => IP6_LEN_V1,
            UdpFlow::V6 {
                extension: Some(ext),
                ..
            } => IP6_LEN_V2 + ext.flagged_len(),
        }
    }

    /// Every flag's fields at once, on top of the largest base footer.
    const MAX_FLAGGED_LEN: usize = TARGET_PORT_LEN;

    pub const MAX_IP4_LEN: usize = {
        m_max_list(&[IP4_LEN_V1, IP4_LEN_V2_WITH_FRAG, IP4_LEN_V2_WITHOUT_FRAG])
            + Self::MAX_FLAGGED_LEN
    };

    pub const MAX_IP6_LEN: usize =
        { m_max_list(&[IP6_LEN_V1, IP6_LEN_V2]) + Self::MAX_FLAGGED_LEN };

    pub const MX_LEN: usize = { m_max_list(&[Self::MAX_IP4_LEN, Self::MAX_IP6_LEN]) };
}

#[cfg(test)]
mod test {
    use std::num::NonZeroU64;

    use super::{FooterFlags, UdpFlow, UdpFlowExtension};

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
                target_port: 0,
            }),
        };

        flow.write_to(&mut data[100..]);

        let parsed = UdpFlow::from_tail(&data[..100 + flow.footer_len()]).unwrap();
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
                target_port: 0,
            }),
        };

        flow.write_to(&mut data[100..]);

        let parsed = UdpFlow::from_tail(&data[..100 + flow.footer_len()]).unwrap();
        assert_eq!(flow, parsed);
    }

    /// A set target port raises the TARGET_PORT flag in the footer id; a zero
    /// one keeps the plain V2 footer old agents understand. Unknown flags are
    /// refused rather than guessed at.
    #[test]
    fn target_port_selects_footer_flags() {
        let extension = UdpFlowExtension {
            port_offset: 0,
            tunnel_id: NonZeroU64::new(7).unwrap(),
            client_server_id: NonZeroU64::new(9).unwrap(),
            target_port: 19140,
        };
        let v2 = UdpFlowExtension {
            target_port: 0,
            ..extension
        };

        let flows = [
            UdpFlow::V4 {
                src: "4.2.1.3:1234".parse().unwrap(),
                dst: "1.2.3.4:5512".parse().unwrap(),
                frag: None,
                extension: Some(extension),
            },
            UdpFlow::V4 {
                src: "4.2.1.3:1234".parse().unwrap(),
                dst: "1.2.3.4:5512".parse().unwrap(),
                frag: Some(super::FragmentInfo {
                    packet_id: std::num::NonZeroU16::new(5).unwrap(),
                    frag_offset: 8,
                    has_more: true,
                }),
                extension: Some(extension),
            },
            UdpFlow::V6 {
                src: ("::1".parse().unwrap(), 100),
                dst: ("::2".parse().unwrap(), 999),
                extension: Some(extension),
            },
        ];

        for flow in flows {
            let mut data = vec![0u8; 256];
            assert!(flow.write_to(&mut data[100..]));
            let len = flow.footer_len();
            assert_eq!(UdpFlow::from_tail(&data[..100 + len]), Ok(flow));

            /* the same flow without a target port is two bytes shorter and still round-trips */
            let mut short = flow;
            match &mut short {
                UdpFlow::V4 { extension, .. } | UdpFlow::V6 { extension, .. } => {
                    *extension = Some(v2)
                }
            }
            assert_eq!(short.footer_len(), len - 2);
            let mut data = vec![0u8; 256];
            assert!(short.write_to(&mut data[100..]));
            assert_eq!(UdpFlow::from_tail(&data[..100 + len - 2]), Ok(short));

            /* the flagged id is the V2 magic plus the flag; an unknown flag is an error */
            let mut data = vec![0u8; 256];
            assert!(flow.write_to(&mut data[100..]));
            let end = 100 + len;
            let id = u64::from_be_bytes(data[end - 8..end].try_into().unwrap());
            assert_eq!(id & FooterFlags::MASK, FooterFlags::HAS_TARGET_PORT);
            data[end - 8..end].copy_from_slice(&(id | 0x80).to_be_bytes());
            assert_eq!(UdpFlow::from_tail(&data[..end]), Err(Some(id | 0x80)));
        }
    }
}
