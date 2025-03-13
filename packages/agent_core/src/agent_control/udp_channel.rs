use std::collections::VecDeque;
use std::fmt::Display;
use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::RwLock;

use playit_agent_proto::control_messages::UdpChannelDetails;

use crate::agent_control::udp_proto::{UDP_CHANNEL_ESTABLISH_ID, UdpFlow};
use crate::utils::now_sec;

use super::PacketTx;

pub struct UdpChannel {
    inner: Arc<Inner>,
}

impl Clone for UdpChannel {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

struct Inner {
    details: RwLock<ChannelDetails>,
    last_confirm: AtomicU32,
    last_send: AtomicU32,
}

struct ChannelDetails {
    udp: Option<UdpChannelDetails>,
    addr_history: VecDeque<SocketAddr>,
}

impl UdpChannel {
    pub fn new() -> Self {
        UdpChannel {
            inner: Arc::new(Inner {
                details: RwLock::new(ChannelDetails {
                    udp: None,
                    addr_history: VecDeque::new(),
                }),
                last_confirm: AtomicU32::new(0),
                last_send: AtomicU32::new(0),
            })
        }
    }

    pub async fn is_setup(&self) -> bool {
        self.inner.details.read().await.udp.is_some()
    }

    pub fn invalidate_session(&self) {
        self.inner.last_confirm.store(0, Ordering::SeqCst);
        self.inner.last_send.store(0, Ordering::SeqCst);
    }

    pub fn check_resend(&self, now_sec: u32) -> bool {
        let last_confirm = self.inner.last_confirm.load(Ordering::SeqCst);
        let last_send = self.inner.last_send.load(Ordering::SeqCst);

        /* if last confirm is 10s old, send keep alive */
        let resend = (last_confirm + 10).max(last_send + 5) < now_sec;

        if resend {
            self.inner.last_send.store(now_sec, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub fn requires_auth(&self) -> bool {
        let last_confirm = self.inner.last_confirm.load(Ordering::SeqCst);
        let last_send = self.inner.last_send.load(Ordering::SeqCst);
        
        /* timeout of 8s for receiving confirm */
        last_confirm + 8 < last_send
    }

    pub async fn resend_token<I: PacketTx>(&self, io: &I) -> std::io::Result<bool> {
        let token = {
            let lock = self.inner.details.read().await;
            match &lock.udp {
                Some(v) => v.clone(),
                None => return Ok(false),
            }
        };

        self.send_token(&token, io).await?;
        Ok(true)
    }

    pub async fn send_token<I: PacketTx>(&self, details: &UdpChannelDetails, io: &I) -> std::io::Result<()> {
        /* add new tunnel address to history so we accept replies */
        {
            let mut lock = self.inner.details.write().await;
            let same_tunnel_addr = lock.udp.as_ref()
                .map(|v| v.tunnel_addr.eq(&details.tunnel_addr))
                .unwrap_or(false);

            if !same_tunnel_addr && !lock.addr_history.contains(&details.tunnel_addr) {
                tracing::info!(tunnel_addr = %details.tunnel_addr, "add tunnel address to history");

                lock.addr_history.push_front(details.tunnel_addr);
                if 8 < lock.addr_history.len() {
                    let _ = lock.addr_history.pop_back();
                }
            }
        }

        io.send_to(&details.token, details.tunnel_addr).await?;

        tracing::info!(token_len = details.token.len(), tunnel_addr = %details.tunnel_addr, "send udp session token");
        self.inner.last_send.store(now_sec(), Ordering::SeqCst);

        Ok(())
    }

    pub async fn send_host_pkt<I: PacketTx>(&self, data: &mut [u8], data_len: usize, flow: UdpFlow, io: &I) -> std::io::Result<usize> {
        assert!(data_len <= data.len());

        let details = self.get_details().await?;

        let flow_len = flow.footer_len();
        let updated_len = data_len + flow_len;

        if !flow.write_to(&mut data[data_len..]) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "data buffer not large enough for flow footer"));
        }

        io.send_to(&data[..updated_len], details.tunnel_addr).await
    }

    async fn get_details(&self) -> Result<UdpChannelDetails, UdpChannelError> {
        let lock = self.inner.details.read().await;

        let details = match &lock.udp {
            Some(v) => v,
            None => return Err(UdpChannelError::UdpTunnelNotConnected),
        };

        Ok(details.clone())
    }

    pub async fn parse_packet(&self, buffer: &[u8], bytes: usize, remote: SocketAddr) -> Result<UdpTunnelRx, UdpChannelError> {
        'check_origin: {
            let lock = self.inner.details.read().await;

            if let Some(udp) = &lock.udp {
                if udp.tunnel_addr == remote {
                    break 'check_origin;
                }
            }

            if !lock.addr_history.contains(&remote) {
                return Err(UdpChannelError::InvalidSource(remote));
            }
        }

        match UdpFlow::from_tail(&buffer[..bytes]) {
            Ok(flow) => return Ok(UdpTunnelRx::ReceivedPacket {
                bytes: bytes - flow.footer_len(),
                flow,
            }),
            Err(Some(footer)) if footer == UDP_CHANNEL_ESTABLISH_ID => {}
            Err(Some(_)) => return Err(UdpChannelError::InvalidFooter),
            Err(None) => return Err(UdpChannelError::InvalidFooter),
        }

        self.inner.last_confirm.store(now_sec(), Ordering::SeqCst);

        let mut lock = self.inner.details.write().await;
        match &lock.udp {
            Some(current) if buffer[..bytes].eq(&current.token[..]) => {
                tracing::info!(token_len = bytes, tunnel_addr = %remote, "udp session confirmed");
                Ok(UdpTunnelRx::ConfirmedConnection)
            }
            _ => {
                tracing::info!(token_len = bytes, tunnel_addr = %remote, "udp session updated");
                let old = lock.udp.replace(UdpChannelDetails {
                    tunnel_addr: remote,
                    token: Arc::new(buffer[..bytes].to_vec()),
                });

                if let Some(old) = old {
                    if old.tunnel_addr != remote {
                        tracing::info!(tunnel_addr = %old.tunnel_addr, "saving old tunnel address");

                        lock.addr_history.push_front(old.tunnel_addr);
                        if 8 < lock.addr_history.len() {
                            let _ = lock.addr_history.pop_back();
                        }
                    }
                }

                Ok(UdpTunnelRx::UpdatedConnection)
            }
        }
    }
}

#[derive(Debug)]
pub enum UdpChannelError {
    InvalidSource(SocketAddr),
    ReceiveBufferTooSmall,
    InvalidFooter,
    UdpTunnelNotConnected,
}

impl Display for UdpChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for UdpChannelError {
}

impl From<UdpChannelError> for std::io::Error {
    fn from(value: UdpChannelError) -> Self {
        let kind = match &value {
            UdpChannelError::InvalidSource(_) => std::io::ErrorKind::InvalidInput,
            UdpChannelError::ReceiveBufferTooSmall => std::io::ErrorKind::UnexpectedEof,
            UdpChannelError::InvalidFooter => std::io::ErrorKind::InvalidData,
            UdpChannelError::UdpTunnelNotConnected => std::io::ErrorKind::NotConnected,
        };

        std::io::Error::new(kind, value)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum UdpTunnelRx {
    ReceivedPacket {
        bytes: usize,
        flow: UdpFlow,
    },
    ConfirmedConnection,
    UpdatedConnection,
}

#[derive(Debug, PartialEq, Eq)]
pub struct UdpExtension {
    pub tunnel_id: u64,
    pub client_server_id: u64,
}

