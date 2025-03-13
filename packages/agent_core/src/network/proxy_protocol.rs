use std::{io::Write, net::{Ipv4Addr, Ipv6Addr}};

use byteorder::{BigEndian, ReadBytesExt};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::{agent_control::udp_proto::UdpFlow, utils::ip_bytes::ReadIpBytesExt};

/*
 DOCS: https://www.haproxy.org/download/1.8/doc/proxy-protocol.txt
*/


#[derive(PartialEq, Eq, Debug)]
pub enum ProxyProtocolHeader {
    AfInet {
        client_ip: Ipv4Addr,
        proxy_ip: Ipv4Addr,
        client_port: u16,
        proxy_port: u16,
    },
    AfInet6 {
        client_ip: Ipv6Addr,
        proxy_ip: Ipv6Addr,
        client_port: u16,
        proxy_port: u16,
    },
}

impl ProxyProtocolHeader {
    pub fn from_udp_flow(flow: &UdpFlow) -> Self {
        match flow {
            UdpFlow::V4 { src, dst, .. } => {
                ProxyProtocolHeader::AfInet {
                    client_ip: *src.ip(),
                    proxy_ip: *dst.ip(),
                    client_port: src.port(),
                    proxy_port: dst.port(),
                }
            }
            UdpFlow::V6 { src, dst, .. } => {
                ProxyProtocolHeader::AfInet6 {
                    client_ip: src.0.into(),
                    proxy_ip: dst.0.into(),
                    client_port: src.1,
                    proxy_port: dst.1,
                }
            }
        }
    }
}

impl std::fmt::Display for ProxyProtocolHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AfInet { client_ip, proxy_ip, client_port, proxy_port } => {
                write!(f, "PROXY TCP4 {client_ip} {proxy_ip} {client_port} {proxy_port}\r\n")
            }
            Self::AfInet6 { client_ip, proxy_ip, client_port, proxy_port } => {
                write!(f, "PROXY TCP6 {client_ip} {proxy_ip} {client_port} {proxy_port}\r\n")
            }
        }
    }
}

const PROXY_PROTOCOL_V2_HEADER: &'static [u8] = &[
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
    /* version 2 + proxy connection byte */ 0x21
];

pub const UDP_PROXY_PROTOCOL_LEN_V6: usize = 16 + 36;
pub const UDP_PROXY_PROTOCOL_LEN_V4: usize = 16 + 12;
pub const UDP_PROXY_PROTOCOL_MAX_LEN: usize = UDP_PROXY_PROTOCOL_LEN_V6;

impl ProxyProtocolHeader {
    pub async fn write_v1_tcp<W: AsyncWrite + Unpin>(&self, out: &mut W) -> Result<(), std::io::Error> {
        out.write_all(self.to_string().as_bytes()).await
    }

    pub async fn write_v2_tcp<W: AsyncWrite + Unpin>(&self, out: &mut W) -> Result<(), std::io::Error> {
        out.write_all(PROXY_PROTOCOL_V2_HEADER).await?;

        match self {
            Self::AfInet { client_ip, proxy_ip, client_port, proxy_port } => {
                out.write_all(&[ /* TCP4: AF_INET + STREAM */ 0x11 ]).await?;
                out.write_all(/* length */ &12u16.to_be_bytes()).await?;
                out.write_all(&client_ip.octets()).await?;
                out.write_all(&proxy_ip.octets()).await?;
                out.write_all(&client_port.to_be_bytes()).await?;
                out.write_all(&proxy_port.to_be_bytes()).await?;
            }
            Self::AfInet6 { client_ip, proxy_ip, client_port, proxy_port } => {
                out.write_all(&[ /* TCP6: AF_INET6 + STREAM */ 0x21 ]).await?;
                out.write_all(/* length */ &36u16.to_be_bytes()).await?;
                out.write_all(&client_ip.octets()).await?;
                out.write_all(&proxy_ip.octets()).await?;
                out.write_all(&client_port.to_be_bytes()).await?;
                out.write_all(&proxy_port.to_be_bytes()).await?;
            }
        }

        Ok(())
    }

    pub fn write_v2_udp<W: Write>(&self, out: &mut W) -> Result<(), std::io::Error> {
        out.write_all(PROXY_PROTOCOL_V2_HEADER)?;

        match self {
            Self::AfInet { client_ip, proxy_ip, client_port, proxy_port } => {
                out.write_all(&[ /* UDP4: AF_INET + DGRAM */ 0x12 ])?;
                out.write_all(/* length */ &12u16.to_be_bytes())?;
                out.write_all(&client_ip.octets())?;
                out.write_all(&proxy_ip.octets())?;
                out.write_all(&client_port.to_be_bytes())?;
                out.write_all(&proxy_port.to_be_bytes())?;
            }
            Self::AfInet6 { client_ip, proxy_ip, client_port, proxy_port } => {
                out.write_all(&[ /* UDP6: AF_INET6 + DGRAM */ 0x22 ])?;
                out.write_all(/* length */ &36u16.to_be_bytes())?;
                out.write_all(&client_ip.octets())?;
                out.write_all(&proxy_ip.octets())?;
                out.write_all(&client_port.to_be_bytes())?;
                out.write_all(&proxy_port.to_be_bytes())?;
            }
        }

        Ok(())
    }

    pub fn parse_v2_udp<R: std::io::Read>(buffer: &mut R) -> Option<Self> {
        let mut header = [0u8; PROXY_PROTOCOL_V2_HEADER.len()];
        buffer.read_exact(&mut header).ok()?;

        if !header.eq(PROXY_PROTOCOL_V2_HEADER) {
            return None;
        }

        let proto_type = buffer.read_u8().ok()?;
        match proto_type {
            /* AF_INET */
            0x12 => {
                let mut slab = [0u8; 14];
                buffer.read_exact(&mut slab).ok()?;
                let mut reader = &slab[..];

                /* length */
                if reader.read_u16::<BigEndian>().unwrap() != 12 {
                    return None;
                }

                let client_ip = reader.read_ip4().unwrap();
                let proxy_ip = reader.read_ip4().unwrap();
                let client_port = reader.read_u16::<BigEndian>().unwrap();
                let proxy_port = reader.read_u16::<BigEndian>().unwrap();

                Some(ProxyProtocolHeader::AfInet {
                    client_ip,
                    proxy_ip,
                    client_port,
                    proxy_port,
                })
            }
            /* AF_INET6 */
            0x22 => {
                let mut slab = [0u8; 38];
                buffer.read_exact(&mut slab).ok()?;
                let mut reader = &slab[..];

                if reader.read_u16::<BigEndian>().unwrap() != 36 {
                    return None;
                }

                let client_ip = reader.read_ip6().unwrap();
                let proxy_ip = reader.read_ip6().unwrap();
                let client_port = reader.read_u16::<BigEndian>().unwrap();
                let proxy_port = reader.read_u16::<BigEndian>().unwrap();

                Some(ProxyProtocolHeader::AfInet6 {
                    client_ip,
                    proxy_ip,
                    client_port,
                    proxy_port,
                })
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod test {
    use super::ProxyProtocolHeader;

    #[test]
    fn test_parse_header() {
        let mut buffer = Vec::new();
        let header = ProxyProtocolHeader::AfInet {
            client_ip: "123.45.12.34".parse().unwrap(),
            proxy_ip: "5.6.7.8".parse().unwrap(),
            client_port: 421,
            proxy_port: 662
        };

        header.write_v2_udp(&mut buffer).unwrap();

        let mut reader = &buffer[..];
        let parsed = ProxyProtocolHeader::parse_v2_udp(&mut reader).unwrap();
        assert_eq!(header, parsed);
    }
}
