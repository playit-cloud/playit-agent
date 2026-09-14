//! HAProxy PROXY protocol headers, versions 1 and 2.
//! See <https://www.haproxy.org/download/1.8/doc/proxy-protocol.txt>.

use std::{
    io::{Read, Write},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
};

use byteorder::{BigEndian, ReadBytesExt};
use playit_agent_proto::udp_proto::UdpFlow;
use tokio::io::{AsyncWrite, AsyncWriteExt};

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
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

const V2_SIGNATURE: &[u8] = &[
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
    /* version 2, PROXY command */ 0x21,
];

pub const UDP_PROXY_PROTOCOL_LEN_V4: usize = 16 + 12;
pub const UDP_PROXY_PROTOCOL_LEN_V6: usize = 16 + 36;
pub const UDP_PROXY_PROTOCOL_MAX_LEN: usize = UDP_PROXY_PROTOCOL_LEN_V6;

impl ProxyProtocolHeader {
    /// `None` if the client and proxy addresses are of different families.
    pub fn from_addrs(client: SocketAddr, proxy: SocketAddr) -> Option<Self> {
        match (client, proxy) {
            (SocketAddr::V4(client), SocketAddr::V4(proxy)) => Some(Self::AfInet {
                client_ip: *client.ip(),
                proxy_ip: *proxy.ip(),
                client_port: client.port(),
                proxy_port: proxy.port(),
            }),
            (SocketAddr::V6(client), SocketAddr::V6(proxy)) => Some(Self::AfInet6 {
                client_ip: *client.ip(),
                proxy_ip: *proxy.ip(),
                client_port: client.port(),
                proxy_port: proxy.port(),
            }),
            _ => None,
        }
    }

    pub fn from_udp_flow(flow: &UdpFlow) -> Self {
        match flow {
            UdpFlow::V4 { src, dst, .. } => Self::AfInet {
                client_ip: *src.ip(),
                proxy_ip: *dst.ip(),
                client_port: src.port(),
                proxy_port: dst.port(),
            },
            UdpFlow::V6 { src, dst, .. } => Self::AfInet6 {
                client_ip: src.0,
                proxy_ip: dst.0,
                client_port: src.1,
                proxy_port: dst.1,
            },
        }
    }

    pub async fn write_v1_tcp<W: AsyncWrite + Unpin>(&self, out: &mut W) -> std::io::Result<()> {
        out.write_all(self.to_string().as_bytes()).await
    }

    pub async fn write_v2_tcp<W: AsyncWrite + Unpin>(&self, out: &mut W) -> std::io::Result<()> {
        let mut buffer = Vec::with_capacity(UDP_PROXY_PROTOCOL_MAX_LEN);
        self.write_v2(&mut buffer, Transport::Stream)?;
        out.write_all(&buffer).await
    }

    pub fn write_v2_udp<W: Write>(&self, out: &mut W) -> std::io::Result<()> {
        self.write_v2(out, Transport::Datagram)
    }

    fn write_v2<W: Write>(&self, out: &mut W, transport: Transport) -> std::io::Result<()> {
        out.write_all(V2_SIGNATURE)?;

        match self {
            Self::AfInet {
                client_ip,
                proxy_ip,
                client_port,
                proxy_port,
            } => {
                out.write_all(&[0x10 | transport as u8])?;
                out.write_all(&12u16.to_be_bytes())?;
                out.write_all(&client_ip.octets())?;
                out.write_all(&proxy_ip.octets())?;
                out.write_all(&client_port.to_be_bytes())?;
                out.write_all(&proxy_port.to_be_bytes())?;
            }
            Self::AfInet6 {
                client_ip,
                proxy_ip,
                client_port,
                proxy_port,
            } => {
                out.write_all(&[0x20 | transport as u8])?;
                out.write_all(&36u16.to_be_bytes())?;
                out.write_all(&client_ip.octets())?;
                out.write_all(&proxy_ip.octets())?;
                out.write_all(&client_port.to_be_bytes())?;
                out.write_all(&proxy_port.to_be_bytes())?;
            }
        }

        Ok(())
    }

    pub fn parse_v2_udp<R: Read>(buffer: &mut R) -> Option<Self> {
        let mut signature = [0u8; V2_SIGNATURE.len()];
        buffer.read_exact(&mut signature).ok()?;
        if signature != V2_SIGNATURE {
            return None;
        }

        match buffer.read_u8().ok()? {
            0x12 => {
                let mut body = [0u8; 14];
                buffer.read_exact(&mut body).ok()?;
                let mut reader = &body[..];

                if reader.read_u16::<BigEndian>().ok()? != 12 {
                    return None;
                }

                Some(Self::AfInet {
                    client_ip: read_ip4(&mut reader)?,
                    proxy_ip: read_ip4(&mut reader)?,
                    client_port: reader.read_u16::<BigEndian>().ok()?,
                    proxy_port: reader.read_u16::<BigEndian>().ok()?,
                })
            }
            0x22 => {
                let mut body = [0u8; 38];
                buffer.read_exact(&mut body).ok()?;
                let mut reader = &body[..];

                if reader.read_u16::<BigEndian>().ok()? != 36 {
                    return None;
                }

                Some(Self::AfInet6 {
                    client_ip: read_ip6(&mut reader)?,
                    proxy_ip: read_ip6(&mut reader)?,
                    client_port: reader.read_u16::<BigEndian>().ok()?,
                    proxy_port: reader.read_u16::<BigEndian>().ok()?,
                })
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum Transport {
    Stream = 0x1,
    Datagram = 0x2,
}

fn read_ip4<R: Read>(reader: &mut R) -> Option<Ipv4Addr> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes).ok()?;
    Some(Ipv4Addr::from(bytes))
}

fn read_ip6<R: Read>(reader: &mut R) -> Option<Ipv6Addr> {
    let mut bytes = [0u8; 16];
    reader.read_exact(&mut bytes).ok()?;
    Some(Ipv6Addr::from(bytes))
}

impl std::fmt::Display for ProxyProtocolHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AfInet {
                client_ip,
                proxy_ip,
                client_port,
                proxy_port,
            } => write!(
                f,
                "PROXY TCP4 {client_ip} {proxy_ip} {client_port} {proxy_port}\r\n"
            ),
            Self::AfInet6 {
                client_ip,
                proxy_ip,
                client_port,
                proxy_port,
            } => write!(
                f,
                "PROXY TCP6 {client_ip} {proxy_ip} {client_port} {proxy_port}\r\n"
            ),
        }
    }
}

#[cfg(test)]
mod test {
    use super::ProxyProtocolHeader;

    #[test]
    fn v2_udp_round_trip() {
        let header = ProxyProtocolHeader::AfInet {
            client_ip: "123.45.12.34".parse().unwrap(),
            proxy_ip: "5.6.7.8".parse().unwrap(),
            client_port: 421,
            proxy_port: 662,
        };

        let mut buffer = Vec::new();
        header.write_v2_udp(&mut buffer).unwrap();
        assert_eq!(buffer.len(), super::UDP_PROXY_PROTOCOL_LEN_V4);

        let parsed = ProxyProtocolHeader::parse_v2_udp(&mut &buffer[..]).unwrap();
        assert_eq!(header, parsed);
    }

    #[test]
    fn v2_udp_round_trip_ip6() {
        let header = ProxyProtocolHeader::AfInet6 {
            client_ip: "2001:db8::1".parse().unwrap(),
            proxy_ip: "2001:db8::2".parse().unwrap(),
            client_port: 1,
            proxy_port: 2,
        };

        let mut buffer = Vec::new();
        header.write_v2_udp(&mut buffer).unwrap();
        assert_eq!(buffer.len(), super::UDP_PROXY_PROTOCOL_LEN_V6);

        let parsed = ProxyProtocolHeader::parse_v2_udp(&mut &buffer[..]).unwrap();
        assert_eq!(header, parsed);
    }

    #[tokio::test]
    async fn v2_tcp_uses_stream_transport_byte() {
        let header = ProxyProtocolHeader::AfInet {
            client_ip: "1.2.3.4".parse().unwrap(),
            proxy_ip: "5.6.7.8".parse().unwrap(),
            client_port: 1,
            proxy_port: 2,
        };

        let mut tcp = Vec::new();
        header.write_v2_tcp(&mut tcp).await.unwrap();
        let mut udp = Vec::new();
        header.write_v2_udp(&mut udp).unwrap();

        assert_eq!(tcp[13], 0x11);
        assert_eq!(udp[13], 0x12);
        assert_eq!(tcp[14..], udp[14..]);
    }

    #[test]
    fn v1_text_format() {
        let header = ProxyProtocolHeader::AfInet {
            client_ip: "1.2.3.4".parse().unwrap(),
            proxy_ip: "5.6.7.8".parse().unwrap(),
            client_port: 1,
            proxy_port: 2,
        };
        assert_eq!(header.to_string(), "PROXY TCP4 1.2.3.4 5.6.7.8 1 2\r\n");
    }
}
