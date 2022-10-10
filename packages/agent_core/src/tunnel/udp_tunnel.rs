use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use playit_agent_proto::control_messages::UdpChannelDetails;

use crate::tunnel::udp_proto::UdpFlow;
use crate::utils::now_milli;

#[derive(Clone, Default)]
pub struct UdpTunnel {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    udp: RwLock<(Option<(UdpSocket, UdpChannelDetails)>)>,
    details: RwLock<Option<UdpChannelDetails>>,
    last_confirm: AtomicU64,
}

impl UdpTunnel {
    pub fn new() -> Self {
        Default::default()
    }

    pub async fn is_setup(&self) -> bool {
        self.inner.details.read().await.is_some()
    }

    pub fn last_confirm(&self) -> u64 {
        self.inner.last_confirm.load(Ordering::SeqCst)
    }

    pub async fn reset(&self) {
        self.inner.last_confirm.store(0, Ordering::SeqCst);
    }

    pub async fn set_udp_tunnel(&self, details: UdpChannelDetails) -> std::io::Result<()> {
        let mut details_lock = self.inner.details.write().await;

        /* if details haven't changed, exit */
        if let Some(current) = &*details_lock {
            if details.eq(current) {
                return Ok(());
            }
        }

        let mut udp_lock = self.inner.udp.write().await;
        match &mut *udp_lock {
            Some((udp, current_details)) => {
                let change_udp = details.tunnel_addr.is_ipv4() != current_details.tunnel_addr.is_ipv4();

                if change_udp {
                    let new_udp = UdpSocket::bind(match current_details.tunnel_addr {
                        SocketAddr::V4(_) => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
                        SocketAddr::V6(_) => SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)),
                    }).await?;
                    new_udp.send_to(&details.token, details.tunnel_addr).await?;

                    *udp = new_udp;
                }

                *current_details = details.clone();
            }
            None => {
                let new_udp = UdpSocket::bind(match details.tunnel_addr {
                    SocketAddr::V4(_) => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
                    SocketAddr::V6(_) => SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)),
                }).await?;
                new_udp.send_to(&details.token, details.tunnel_addr).await?;

                udp_lock.replace((new_udp, details.clone()));
            }
        };

        details_lock.replace(details);
        Ok(())
    }

    pub async fn resend_setup(&self) -> std::io::Result<usize> {
        let udp_lock = self.inner.udp.read().await;
        let (udp, details) = match &*udp_lock {
            Some(v) => v,
            None => return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "udp tunnel not connected")),
        };

        udp.send_to(&details.token, details.tunnel_addr).await
    }

    pub async fn send(&self, data: &mut Vec<u8>, flow: UdpFlow) -> std::io::Result<usize> {
        /* append flow to udp packet */
        let og_packet_len = data.len();
        data.resize(flow.len() + og_packet_len, 0);
        flow.write_to(&mut data[og_packet_len..]);

        /* send packet */
        {
            let udp_lock = self.inner.udp.read().await;
            let (udp, details) = match &*udp_lock {
                Some(v) => v,
                None => return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "udp tunnel not connected")),
            };

            udp.send_to(&data, details.tunnel_addr).await
        }
    }

    pub async fn receive_from(&self, buffer: &mut [u8]) -> std::io::Result<UdpTunnelRx> {
        let ((bytes, remote), details) = {
            let udp_lock = self.inner.udp.read().await;
            let (udp, details) = match &*udp_lock {
                Some(v) => v,
                None => return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "udp tunnel not connected")),
            };

            (udp.recv_from(buffer).await?, details.clone())
        };

        if buffer[..bytes].eq(&details.token[..]) {
            self.inner.last_confirm.store(now_milli(), Ordering::SeqCst);
            return Ok(UdpTunnelRx::ConfirmedConnection);
        }

        if buffer.len() + UdpFlow::len_v4().max(UdpFlow::len_v6()) < bytes {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "receive buffer too small"));
        }

        if details.tunnel_addr != remote {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "got data from other source"));
        }

        let footer = match UdpFlow::from_tail(&buffer[..bytes]) {
            Some(v) => v,
            None => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "failed to extract udp footer")),
        };

        Ok(UdpTunnelRx::ReceivedPacket {
            bytes: bytes - footer.len(),
            flow: footer,
        })
    }
}

pub enum UdpTunnelRx {
    ReceivedPacket { bytes: usize, flow: UdpFlow },
    ConfirmedConnection,
}
