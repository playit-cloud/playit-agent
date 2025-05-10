use std::{future::Future, net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6}, sync::{atomic::AtomicUsize, Arc}, task::Poll};

use playit_agent_proto::control_messages::Pong;
use errors::SetupError;
use tokio::{io::ReadBuf, net::UdpSocket};
use version::get_version;

use playit_api_client::{api::{ReqAgentsRoutingGet, ReqProtoRegister}, PlayitApi};
pub use playit_api_client::api::SignedAgentKey;

use crate::utils::error_helper::ErrorHelper;

pub mod errors;

pub mod address_selector;
pub mod connected_control;
pub mod established_control;
pub mod maintained_control;
pub mod version;
pub mod platform;

pub trait PacketIO: Send + Sync + 'static {
    fn send_to(&self, buf: &[u8], target: SocketAddr) -> impl Future<Output = std::io::Result<usize>> + Sync + Send;

    fn recv_from(&self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Sync + Send;
}

pub trait PacketRx: Send + Sync + 'static {
    fn recv_from(&self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Sync + Send;
}

impl<T: PacketIO> PacketRx for T {
    fn recv_from(&self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Sync + Send {
        T::recv_from(self, buf)
    }
}

impl<T: PacketIO> PacketRx for Arc<T> {
    fn recv_from(&self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Sync + Send {
        T::recv_from(self, buf)
    }
}

pub trait PacketTx {
    fn send_to(&self, buf: &[u8], target: SocketAddr) -> impl Future<Output = std::io::Result<usize>> + Sync + Send;
}

impl<T: PacketIO> PacketTx for T {
    fn send_to(&self, buf: &[u8], target: SocketAddr) -> impl Future<Output = std::io::Result<usize>> + Sync + Send {
        T::send_to(self, buf, target)
    }
}

impl<T: PacketIO> PacketTx for Arc<T> {
    fn send_to(&self, buf: &[u8], target: SocketAddr) -> impl Future<Output = std::io::Result<usize>> + Sync + Send {
        T::send_to(self, buf, target)
    }
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

    pub fn local_ip4_port(&self) -> Option<u16> {
        Some(self.ip4.local_addr().ok()?.port())
    }

    pub fn local_ip6_port(&self) -> Option<u16> {
        Some(self.ip6.as_ref()?.local_addr().ok()?.port())
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

impl Future for PoolBoth<'_> {
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
    client: PlayitApi,
}

impl AuthApi {
    pub fn new(api_url: String, secret_key: String) -> Self {
        let client = PlayitApi::create(
            api_url,
            Some(secret_key)
        );
        AuthApi { client }
    }
}

impl AuthResource for AuthApi {
    async fn authenticate(&self, pong: &Pong) -> Result<SignedAgentKey, SetupError> {
        let res = self.client.proto_register(ReqProtoRegister {
            agent_version: get_version(),
            client_addr: pong.client_addr,
            tunnel_addr: pong.tunnel_addr,
        }).await.with_error(|error| tracing::error!(?error, "failed to sign and register"))?;

        Ok(res)
    }

    async fn get_control_addresses(&self) -> Result<Vec<SocketAddr>, SetupError> {
        let routing = self.client.agents_routing_get(ReqAgentsRoutingGet { agent_id: None }).await?;

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
