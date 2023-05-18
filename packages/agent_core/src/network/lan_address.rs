use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};

use byteorder::{BigEndian, ByteOrder};
use tokio::net::{TcpSocket, TcpStream, UdpSocket};

use crate::utils::shuffle::shuffle;

pub struct LanAddress;

impl LanAddress {
    pub async fn tcp_socket(special_lan_ip: bool, peer: SocketAddr, host: SocketAddr) -> std::io::Result<TcpStream> {
        if host.ip().is_loopback() && special_lan_ip {
            let local_ip = map_to_local_ip4(peer.ip());
            let socket = TcpSocket::new_v4()?;

            match socket.bind(SocketAddrV4::new(local_ip, 0).into()) {
                Err(e) => {
                    tracing::warn!("Failed to bind connection to special local address to support IP based banning: {:?}", e);
                }
                Ok(_) => {
                    match socket.connect(host).await {
                        Err(e) => {
                            tracing::warn!("Failed to establish connection using special lan {} for flow {:?} {:?}", local_ip, (peer, host), e);
                        }
                        v => return v,
                    };
                }
            }
        }

        match TcpStream::connect(host).await {
            Err(e) => {
                tracing::error!("Failed to establish connection for flow {:?} {:?}. Is your server running?", (peer, host), e);
                Err(e)
            }
            v => v,
        }
    }

    pub async fn udp_socket(special_lan_ip: bool, peer: SocketAddr, host: SocketAddr) -> std::io::Result<UdpSocket> {
        if host.ip().is_loopback() && special_lan_ip {
            let local_ip = map_to_local_ip4(peer.ip());
            let local_port = 40000 + (peer.port() % 24000);

            match UdpSocket::bind(SocketAddrV4::new(local_ip, local_port)).await {
                Ok(v) => Ok(v),
                Err(bad_port_error) => match UdpSocket::bind(SocketAddrV4::new(local_ip, 0)).await {
                    Ok(v) => {
                        tracing::warn!("Failed to bind UDP port to {} to have connections survive agent restart: {:?}", local_port, bad_port_error);
                        Ok(v)
                    }
                    Err(bad_local_ip_err) => {
                        let v = UdpSocket::bind(SocketAddrV4::new(0.into(), 0)).await?;
                        tracing::warn!("Failed to bind UDP to special local address, in-game ip banning will not work: {:?}", bad_local_ip_err);
                        Ok(v)
                    }
                }
            }
        } else {
            UdpSocket::bind(SocketAddrV4::new(0.into(), 0)).await
        }
    }
}

fn as_local_masked(mut ip: u32) -> u32 {
    ip = shuffle(ip) & 0x00FFFFFFu32;
    if ip == 0 {
        ip = 1;
    }
    ip | 0x7F000000u32
}

fn map_to_local_ip4(ip: IpAddr) -> Ipv4Addr {
    Ipv4Addr::from(as_local_masked(match ip {
        IpAddr::V4(ip) => u32::from(ip),
        IpAddr::V6(ip) => {
            let bytes = ip.octets();

            shuffle(BigEndian::read_u32(&bytes[..4]))
                ^ shuffle(BigEndian::read_u32(&bytes[4..8]))
                ^ shuffle(BigEndian::read_u32(&bytes[4..8]))
                ^ shuffle(BigEndian::read_u32(&bytes[4..8]))
        }
    }))
}