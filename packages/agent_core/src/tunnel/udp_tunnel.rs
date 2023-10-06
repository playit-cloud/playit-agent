use std::collections::VecDeque;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use playit_agent_proto::control_messages::UdpChannelDetails;


use crate::tunnel::udp_proto::{UDP_CHANNEL_ESTABLISH_ID, UdpFlow};
use crate::utils::now_sec;

#[derive(Clone)]
pub struct UdpTunnel {
    inner: Arc<Inner>,
}

struct Inner {
    udp4: UdpSocket,
    udp6: Option<UdpSocket>,
    details: RwLock<ChannelDetails>,
    last_confirm: AtomicU32,
    last_send: AtomicU32,
}

struct ChannelDetails {
    udp: Option<UdpChannelDetails>,
    addr_history: VecDeque<SocketAddr>,
}

impl UdpTunnel {
    pub async fn new() -> std::io::Result<Self> {
        Ok(UdpTunnel {
            inner: Arc::new(Inner {
                udp4: UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await?,
                udp6: UdpSocket::bind(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)).await.ok(),
                details: RwLock::new(ChannelDetails {
                    udp: None,
                    addr_history: VecDeque::new(),
                }),
                last_confirm: AtomicU32::new(0),
                last_send: AtomicU32::new(0),
            })
        })
    }

    pub async fn is_setup(&self) -> bool {
        self.inner.details.read().await.udp.is_some()
    }

    pub fn invalidate_session(&self) {
        self.inner.last_confirm.store(0, Ordering::SeqCst);
        self.inner.last_send.store(0, Ordering::SeqCst);
    }

    pub fn requires_resend(&self) -> bool {
        let last_confirm = self.inner.last_confirm.load(Ordering::SeqCst);
        /* send token every 10 seconds */
        10 < now_sec() - last_confirm
    }

    pub fn requires_auth(&self) -> bool {
        let last_confirm = self.inner.last_confirm.load(Ordering::SeqCst);
        let last_send = self.inner.last_send.load(Ordering::SeqCst);

        /* send is confirmed */
        if last_send < last_confirm {
            return false;
        }

        let now = now_sec();
        5 < now - last_send
    }

    pub async fn set_udp_tunnel(&self, details: UdpChannelDetails) -> std::io::Result<()> {
        {
            let mut lock = self.inner.details.write().await;

            /* if details haven't changed, exit */
            if let Some(current) = &lock.udp {
                if details.eq(current) {
                    return Ok(());
                }

                if !details.tunnel_addr.eq(&current.tunnel_addr) {
                    tracing::info!(old = %current.tunnel_addr, new = %details.tunnel_addr, "change udp tunnel addr");

                    let old_addr = current.tunnel_addr;
                    lock.addr_history.push_front(old_addr);

                    if lock.addr_history.len() > 5 {
                        lock.addr_history.pop_back();
                    }
                }
            }

            lock.udp = Some(details.clone());
        }

        self.send_token(&details).await
    }

    pub async fn resend_token(&self) -> std::io::Result<bool> {
        let token = {
            let lock = self.inner.details.read().await;
            match &lock.udp {
                Some(v) => v.clone(),
                None => return Ok(false),
            }
        };

        self.send_token(&token).await?;
        Ok(true)
    }

    async fn send_token(&self, details: &UdpChannelDetails) -> std::io::Result<()> {
        match details.tunnel_addr {
            SocketAddr::V4(tunnel_addr) => {
                self.inner.udp4.send_to(&details.token, tunnel_addr).await?;
            }
            SocketAddr::V6(tunnel_addr) => {
                let udp = match &self.inner.udp6 {
                    Some(v) => v,
                    None => return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "IPv6 not supported")),
                };

                udp.send_to(&details.token, tunnel_addr).await?;
            }
        }

        tracing::info!(token_len = details.token.len(), tunnel_addr = %details.tunnel_addr, "send udp session token");
        self.inner.last_send.store(now_sec(), Ordering::SeqCst);
        Ok(())
    }

    pub async fn send(&self, data: &mut Vec<u8>, flow: UdpFlow) -> std::io::Result<usize> {
        /* append flow to udp packet */
        let og_packet_len = data.len();
        data.resize(flow.len() + og_packet_len, 0);
        flow.write_to(&mut data[og_packet_len..]);

        let (socket, tunnel_addr) = self.get_sock().await?;
        socket.send_to(&data, tunnel_addr).await
    }

    async fn get_sock(&self) -> std::io::Result<(&UdpSocket, SocketAddr)> {
        let lock = self.inner.details.read().await;

        let details = match &lock.udp {
            Some(v) => v,
            None => return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "udp tunnel not connected")),
        };

        Ok(if details.tunnel_addr.is_ipv4() {
            (&self.inner.udp4, details.tunnel_addr)
        } else {
            let Some(udp) = self.inner.udp6.as_ref() else { return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "IPv6 not setup")) };
            (udp, details.tunnel_addr)
        })
    }

    async fn get_token(&self) -> std::io::Result<Arc<Vec<u8>>> {
        let lock = self.inner.details.read().await;

        let details = match &lock.udp {
            Some(v) => v,
            None => return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "udp tunnel not connected")),
        };

        Ok(details.token.clone())
    }

    pub async fn receive_from(&self, buffer: &mut [u8]) -> std::io::Result<UdpTunnelRx> {
        let (udp, tunnel_addr) = self.get_sock().await?;
        let (bytes, remote) = udp.recv_from(buffer).await?;

        if tunnel_addr != remote {
            let lock = self.inner.details.read().await;
            let mut found = false;
            for addr in &lock.addr_history {
                if remote.eq(addr) {
                    found = true;
                    break;
                }
            }

            if !found {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "got data from other source"));
            }
        }

        let token = self.get_token().await?;
        if buffer[..bytes].eq(&token[..]) {
            tracing::info!(token_len = bytes, tunnel_addr = %remote, "udp session confirmed");
            self.inner.last_confirm.store(now_sec(), Ordering::SeqCst);
            return Ok(UdpTunnelRx::ConfirmedConnection);
        }

        if buffer.len() + UdpFlow::len_v4().max(UdpFlow::len_v6()) < bytes {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "receive buffer too small"));
        }

        let footer = match UdpFlow::from_tail(&buffer[..bytes]) {
            Ok(v) => v,
            Err(Some(footer)) if footer == UDP_CHANNEL_ESTABLISH_ID => {
                let actual = hex::encode(&buffer[..bytes]);
                let expected = hex::encode(&token[..]);

                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unexpected UDP establish packet, actual: {}, expected: {}", actual, expected)
                ));
            },
            _ => return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to extract udp footer: {}", hex::encode(&buffer[..bytes]))
            )),
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
