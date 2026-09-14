use std::{
    future::poll_fn,
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
};

use tokio::{io::ReadBuf, net::UdpSocket};

/// One IPv4 socket plus an optional IPv6 socket, both bound to an ephemeral port.
///
/// Datagrams are sent from the socket matching the target family. Receives poll both
/// sockets, alternating which is checked first so neither can starve the other.
pub struct DualStackUdpSocket {
    ip4: UdpSocket,
    ip6: Option<UdpSocket>,
    ip6_first: AtomicBool,
}

impl DualStackUdpSocket {
    pub async fn new() -> io::Result<Self> {
        let ip4 = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await?;
        let ip6 = match UdpSocket::bind(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)).await {
            Ok(socket) => Some(socket),
            Err(error) => {
                tracing::debug!(?error, "ipv6 socket unavailable, using ipv4 only");
                None
            }
        };

        Ok(DualStackUdpSocket {
            ip4,
            ip6,
            ip6_first: AtomicBool::new(false),
        })
    }

    pub fn local_ip4_port(&self) -> Option<u16> {
        self.ip4.local_addr().ok().map(|addr| addr.port())
    }

    pub fn local_ip6_port(&self) -> Option<u16> {
        self.ip6.as_ref()?.local_addr().ok().map(|addr| addr.port())
    }

    pub async fn send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        match (&self.ip6, target) {
            (Some(ip6), SocketAddr::V6(_)) => ip6.send_to(buf, target).await,
            _ => self.ip4.send_to(buf, target).await,
        }
    }

    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let ip6_first = self.ip6_first.fetch_xor(true, Ordering::Relaxed);
        let order = if ip6_first {
            [self.ip6.as_ref(), Some(&self.ip4)]
        } else {
            [Some(&self.ip4), self.ip6.as_ref()]
        };

        poll_fn(|cx| {
            let mut read = ReadBuf::new(buf);
            for socket in order.into_iter().flatten() {
                if let Poll::Ready(result) = socket.poll_recv_from(cx, &mut read) {
                    return Poll::Ready(result.map(|from| (read.filled().len(), from)));
                }
            }
            Poll::Pending
        })
        .await
    }
}
