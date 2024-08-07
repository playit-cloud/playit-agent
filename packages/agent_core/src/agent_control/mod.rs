use std::{future::Future, net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6}, sync::atomic::AtomicUsize, task::Poll};

use platform::get_platform;
use playit_agent_proto::control_messages::Pong;
use errors::SetupError;
use tokio::{io::ReadBuf, net::UdpSocket};

use crate::{api::{api::{AgentVersion, PlayitAgentVersion, ReqAgentsRoutingGet, ReqProtoRegister, SignedAgentKey}, PlayitApi}, utils::error_helper::ErrorHelper};

pub mod errors;

pub mod address_selector;
pub mod connected_control;
pub mod established_control;
pub mod maintained_control;

pub mod udp_channel;
pub mod udp_proto;
mod platform;

pub trait PacketIO {
    fn send_to(&self, buf: &[u8], target: SocketAddr) -> impl Future<Output = std::io::Result<usize>> + Sync;

    fn recv_from(&self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Sync;
}

pub struct DualStackUdpSocket {
    ip4: UdpSocket,
    ip6: Option<UdpSocket>,
    next: AtomicUsize,
}

impl DualStackUdpSocket {
    pub async fn new() -> std::io::Result<Self> {
        let ip4 = UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))).await?;
        let ip6 = UdpSocket::bind(SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0))).await.ok();

        Ok(DualStackUdpSocket {
            ip4,
            ip6,
            next: AtomicUsize::new(0),
        })
    }
}

impl PacketIO for DualStackUdpSocket {
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> std::io::Result<usize> {
        if target.is_ipv6() {
            if let Some(ip6) = &self.ip6 {
                return ip6.send_to(buf, target).await;
            }
        }
        self.ip4.send_to(buf, target).await
    }

    async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        let sel = self.next.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        
        if sel % 2 == 0 {
            PoolBoth {
                buffer: buf,
                a: self.ip6.as_ref(),
                b: Some(&self.ip4),
            }.await
        } else {
            PoolBoth {
                buffer: buf,
                a: Some(&self.ip4),
                b: self.ip6.as_ref(),
            }.await
        }
    }
}

struct PoolBoth<'a> {
    buffer: &'a mut [u8],
    a: Option<&'a UdpSocket>,
    b: Option<&'a UdpSocket>
}

impl<'a> Future for PoolBoth<'a> {
    type Output = std::io::Result<(usize, SocketAddr)>;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        let PoolBoth {
            buffer,
            a,
            b
        } = &mut *self;

        let mut buf = ReadBuf::new(buffer);

        if let Some(a) = a {
            if let Poll::Ready(ready) = a.poll_recv_from(cx, &mut buf) {
                return match ready {
                    Ok(addr) => Poll::Ready(Ok((buf.filled().len(), addr))),
                    Err(error) => Poll::Ready(Err(error))
                };
            }
        }
        
        if let Some(b) = b {
            if let Poll::Ready(ready) = b.poll_recv_from(cx, &mut buf) {
                return match ready {
                    Ok(addr) => Poll::Ready(Ok((buf.filled().len(), addr))),
                    Err(error) => Poll::Ready(Err(error))
                };
            }
        }

        Poll::Pending
    }
}

impl PacketIO for UdpSocket {
    fn send_to(&self, buf: &[u8], target: SocketAddr) -> impl Future<Output = std::io::Result<usize>> + Sync {
        UdpSocket::send_to(self, buf, target)
    }

    fn recv_from(&self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Sync {
        UdpSocket::recv_from(self, buf)
    }
}

pub trait AuthResource: Clone {
    fn authenticate(&self, pong: &Pong) -> impl Future<Output = Result<SignedAgentKey, SetupError>> + Sync;

    fn get_control_addresses(&self) -> impl Future<Output = Result<Vec<SocketAddr>, SetupError>> + Sync;
}

#[derive(Clone)]
pub struct AuthApi {
    pub api_url: String,
    pub secret_key: String,
}

impl AuthApi {
    pub fn api_client(&self) -> PlayitApi {
        PlayitApi::create(self.api_url.clone(), Some(self.secret_key.clone()))
    }
}

impl AuthResource for AuthApi {
    async fn authenticate(&self, pong: &Pong) -> Result<SignedAgentKey, SetupError> {
        let api = self.api_client();

        let res = api.proto_register(ReqProtoRegister {
            agent_version: PlayitAgentVersion {
                version: AgentVersion {
                    platform: get_platform(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                official: true,
                details_website: None,
            },
            client_addr: pong.client_addr,
            tunnel_addr: pong.tunnel_addr,
        }).await.with_error(|error| tracing::error!(?error, "failed to sign and register"))?;

        Ok(res)
    }

    async fn get_control_addresses(&self) -> Result<Vec<SocketAddr>, SetupError> {
        let api = self.api_client();
        let routing = api.agents_routing_get(ReqAgentsRoutingGet { agent_id: None }).await?;

        let mut addresses = vec![];
        for ip6 in routing.targets6 {
            addresses.push(SocketAddr::new(ip6.into(), 5525));
        }
        for ip4 in routing.targets4 {
            addresses.push(SocketAddr::new(ip4.into(), 5525));
        }

        Ok(addresses)
    }
}