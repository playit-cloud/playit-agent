use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use playit_agent_common::{RpcMessage, TunnelFeed, TunnelRequest};
use playit_agent_common::rpc::SignedRpcRequest;

pub struct TunnelIO {
    udp: UdpSocket,
    dest: SocketAddr,
    id: Arc<AtomicU64>,
    recv_buffer: Mutex<Vec<u8>>,
}

impl TunnelIO {
    pub async fn new(dest: SocketAddr) -> std::io::Result<Self> {
        let udp = UdpSocket::bind(match dest {
            SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(0.into()), 0),
            SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(0.into()), 0),
        }).await?;

        Ok(TunnelIO {
            udp,
            dest,
            id: Arc::new(AtomicU64::new(128)),
            recv_buffer: Mutex::new(vec![0u8; 2048]),
        })
    }

    pub fn request_id(&self) -> &AtomicU64 {
        &self.id
    }

    pub async fn send(&self, content: SignedRpcRequest<TunnelRequest>) -> std::io::Result<RpcMessage<SignedRpcRequest<TunnelRequest>>> {
        let request_id = self.id.fetch_add(1, Ordering::SeqCst);
        let req = RpcMessage { request_id, content };

        self.send_raw(&req).await?;
        Ok(req)
    }

    pub async fn send_raw(&self, request: &RpcMessage<SignedRpcRequest<TunnelRequest>>) -> std::io::Result<()> {
        tracing::debug!(?request, to = ?self.dest, "send request");
        self.udp.send_to(&request.as_payload(), self.dest).await?;
        Ok(())
    }

    pub async fn recv(&self) -> std::io::Result<TunnelFeed> {
        let mut buffer = self.recv_buffer.lock().await;

        let (len, addr) = self.udp.recv_from(&mut buffer).await?;
        if addr != self.dest {
            tracing::warn!("got packet not from control addr");
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message from unknown source"));
        }

        match TunnelFeed::from_slice(&buffer[..len]) {
            Some(feed) => {
                tracing::debug!(?feed, from = ?addr, "received feed");
                Ok(feed)
            }
            None => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "failed to parse TunnelFeed")),
        }
    }
}
