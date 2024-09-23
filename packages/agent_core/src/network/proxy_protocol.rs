use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::io::{AsyncWrite, AsyncWriteExt};

pub enum ProxyProtocolHeader {
    Tcp4 {
        client_ip: Ipv4Addr,
        proxy_ip: Ipv4Addr,
        client_port: u16,
        proxy_port: u16,
    },
    Tcp6 {
        client_ip: Ipv6Addr,
        proxy_ip: Ipv6Addr,
        client_port: u16,
        proxy_port: u16,
    },
}

impl std::fmt::Display for ProxyProtocolHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp4 { client_ip, proxy_ip, client_port, proxy_port } => {
                write!(f, "PROXY TCP4 {client_ip} {proxy_ip} {client_port} {proxy_port}\r\n")
            }
            Self::Tcp6 { client_ip, proxy_ip, client_port, proxy_port } => {
                write!(f, "PROXY TCP6 {client_ip} {proxy_ip} {client_port} {proxy_port}\r\n")
            }
        }
    }
}

const PROXY_PROTOCOL_V2_HEADER: &'static [u8] = &[
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
    /* version 2 + proxy connection byte */ 0x21
];

impl ProxyProtocolHeader {
    pub async fn write_v1<W: AsyncWrite + Unpin>(&self, out: &mut W) -> Result<(), std::io::Error> {
        out.write_all(self.to_string().as_bytes()).await
    }

    pub async fn write_v2<W: AsyncWrite + Unpin>(&self, out: &mut W) -> Result<(), std::io::Error> {
        out.write_all(PROXY_PROTOCOL_V2_HEADER).await?;

        match self {
            Self::Tcp4 { client_ip, proxy_ip, client_port, proxy_port } => {
                out.write_all(&[ /* TCP4: AF_INET + STREAM */ 0x11 ]).await?;
                out.write_all(/* length */ &12u16.to_be_bytes()).await?;
                out.write_all(&client_ip.octets()).await?;
                out.write_all(&proxy_ip.octets()).await?;
                out.write_all(&client_port.to_be_bytes()).await?;
                out.write_all(&proxy_port.to_be_bytes()).await?;
            }
            Self::Tcp6 { client_ip, proxy_ip, client_port, proxy_port } => {
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
}
