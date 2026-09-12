use std::{
    future::Future,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::{Arc, atomic::AtomicUsize},
    task::Poll,
};
use tokio::{io::ReadBuf, net::UdpSocket};

pub trait PacketIO: Send + Sync + 'static {
    fn send_to(
        &self,
        buf: &[u8],
        target: SocketAddr,
    ) -> impl Future<Output = std::io::Result<usize>> + Send;

    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Send;
}

pub trait PacketRx: Send + Sync + 'static {
    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Send;
}

impl<T: PacketIO> PacketRx for T {
    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Send {
        T::recv_from(self, buf)
    }
}

impl<T: PacketIO> PacketRx for Arc<T> {
    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Send {
        T::recv_from(self, buf)
    }
}

pub trait PacketTx {
    fn send_to(
        &self,
        buf: &[u8],
        target: SocketAddr,
    ) -> impl Future<Output = std::io::Result<usize>> + Send;
}

impl<T: PacketIO> PacketTx for T {
    fn send_to(
        &self,
        buf: &[u8],
        target: SocketAddr,
    ) -> impl Future<Output = std::io::Result<usize>> + Send {
        T::send_to(self, buf, target)
    }
}

impl<T: PacketIO> PacketTx for Arc<T> {
    fn send_to(
        &self,
        buf: &[u8],
        target: SocketAddr,
    ) -> impl Future<Output = std::io::Result<usize>> + Send {
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
        let ip4 =
            UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))).await?;
        let ip6 = UdpSocket::bind(SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::UNSPECIFIED,
            0,
            0,
            0,
        )))
        .await
        .ok();

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
        match target {
            SocketAddr::V4(_) => self.ip4.send_to(buf, target).await,
            SocketAddr::V6(_) => match &self.ip6 {
                Some(socket) => socket.send_to(buf, target).await,
                None => Err(std::io::ErrorKind::AddrNotAvailable.into()),
            },
        }
    }

    async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        let sel = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if sel.is_multiple_of(2) {
            ReceiveEither {
                buffer: buf,
                a: self.ip6.as_ref(),
                b: Some(&self.ip4),
            }
            .await
        } else {
            ReceiveEither {
                buffer: buf,
                a: Some(&self.ip4),
                b: self.ip6.as_ref(),
            }
            .await
        }
    }
}

struct ReceiveEither<'a> {
    buffer: &'a mut [u8],
    a: Option<&'a UdpSocket>,
    b: Option<&'a UdpSocket>,
}

impl Future for ReceiveEither<'_> {
    type Output = std::io::Result<(usize, SocketAddr)>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let ReceiveEither { buffer, a, b } = &mut *self;

        let mut buf = ReadBuf::new(buffer);

        if let Some(a) = a
            && let Poll::Ready(ready) = a.poll_recv_from(cx, &mut buf)
        {
            return match ready {
                Ok(addr) => Poll::Ready(Ok((buf.filled().len(), addr))),
                Err(error) => Poll::Ready(Err(error)),
            };
        }

        if let Some(b) = b
            && let Poll::Ready(ready) = b.poll_recv_from(cx, &mut buf)
        {
            return match ready {
                Ok(addr) => Poll::Ready(Ok((buf.filled().len(), addr))),
                Err(error) => Poll::Ready(Err(error)),
            };
        }

        Poll::Pending
    }
}

impl PacketIO for UdpSocket {
    fn send_to(
        &self,
        buf: &[u8],
        target: SocketAddr,
    ) -> impl Future<Output = std::io::Result<usize>> + Send {
        UdpSocket::send_to(self, buf, target)
    }

    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> impl Future<Output = std::io::Result<(usize, SocketAddr)>> + Send {
        UdpSocket::recv_from(self, buf)
    }
}
