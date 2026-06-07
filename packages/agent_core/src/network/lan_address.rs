use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use byteorder::{BigEndian, ByteOrder};
use tokio::net::{TcpSocket, TcpStream, UdpSocket};

use crate::utils::shuffle::shuffle;

pub struct LanAddress;

impl LanAddress {
    pub async fn tcp_socket(
        special_lan_ip: bool,
        peer: SocketAddr,
        host: SocketAddr,
    ) -> std::io::Result<TcpStream> {
        let is_loopback = host.ip().is_loopback();
        if is_loopback && special_lan_ip {
            let local_ip = map_to_local_ip4(peer.ip());
            let socket = TcpSocket::new_v4()?;

            match socket.bind(SocketAddrV4::new(local_ip, 0).into()) {
                Err(e) => {
                    tracing::debug!(
                        "could not bind special loopback address; continuing without per-client loopback IP support: {:?}",
                        e
                    );
                }
                Ok(_) => {
                    match socket.connect(host).await {
                        Err(e) => {
                            tracing::debug!(
                                "could not connect using special loopback address {}; continuing with normal local address for flow {:?}: {:?}",
                                local_ip,
                                (peer, host),
                                e
                            );
                        }
                        v => return v,
                    };
                }
            }
        }

        tracing::debug!(is_loopback, host_ip = %host.ip(), special_lan_ip, "not using special lan address");
        match TcpStream::connect(host).await {
            Err(e) => {
                tracing::error!(
                    "Failed to establish connection for flow {:?} {:?}. Is your server running?",
                    (peer, host),
                    e
                );
                Err(e)
            }
            v => v,
        }
    }

    pub async fn udp_socket(
        special_lan_ip: bool,
        peer: SocketAddr,
        target: SocketAddr,
        tunnel_id: u64,
    ) -> std::io::Result<UdpSocket> {
        let ip_shuffle = shuffle_ip_to_u32(peer.ip());

        /* try to have the same client bind to the same local resource */
        let rand_id = shuffle(peer.port() as u32)
            ^ shuffle((tunnel_id >> 32) as u32)
            ^ shuffle(tunnel_id as u32)
            ^ ip_shuffle;

        let local_port = (2048u32 + rand_id % (u16::MAX as u32 - 2048u32)) as u16;

        if special_lan_ip {
            let local_ip = Ipv4Addr::from(as_local_masked(ip_shuffle));

            match UdpSocket::bind(SocketAddrV4::new(local_ip, local_port)).await {
                Ok(v) => Ok(v),
                Err(bad_port_error) => {
                    match UdpSocket::bind(SocketAddrV4::new(local_ip, 0)).await {
                        Ok(v) => {
                            tracing::debug!(
                                "could not bind preferred UDP source port {}; continuing with a random local port: {:?}",
                                local_port,
                                bad_port_error
                            );
                            Ok(v)
                        }
                        Err(bad_local_ip_err) => {
                            let v = UdpSocket::bind(SocketAddrV4::new(0.into(), 0)).await?;
                            tracing::debug!(
                                "could not bind special loopback address for UDP; continuing without per-client loopback IP support: {:?}",
                                bad_local_ip_err
                            );
                            Ok(v)
                        }
                    }
                }
            }
        } else {
            let bind_addr = match target {
                SocketAddr::V4(_) => {
                    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, local_port))
                }
                SocketAddr::V6(_) => {
                    SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, local_port, 0, 0))
                }
            };
            let fallback_addr = match target {
                SocketAddr::V4(_) => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
                SocketAddr::V6(_) => {
                    SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0))
                }
            };

            match UdpSocket::bind(bind_addr).await {
                Ok(v) => Ok(v),
                Err(bad_port_error) => {
                    let v = UdpSocket::bind(fallback_addr).await?;
                    tracing::debug!(
                        "could not bind preferred UDP source port; continuing with a random local port: {:?}",
                        bad_port_error
                    );
                    Ok(v)
                }
            }
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
    Ipv4Addr::from(as_local_masked(shuffle_ip_to_u32(ip)))
}

fn shuffle_ip_to_u32(ip: IpAddr) -> u32 {
    match ip {
        IpAddr::V4(ip) => u32::from(ip),
        IpAddr::V6(ip) => {
            let bytes = ip.octets();

            shuffle(BigEndian::read_u32(&bytes[..4]))
                ^ shuffle(BigEndian::read_u32(&bytes[4..8]))
                ^ shuffle(BigEndian::read_u32(&bytes[8..12]))
                ^ shuffle(BigEndian::read_u32(&bytes[12..16]))
        }
    }
}
