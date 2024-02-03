use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::Error;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::RwLock;

use playit_agent_proto::control_feed::NewClient;

use crate::api::api::AgentTunnel;
use crate::match_ip::MatchIp;
use crate::tunnel::tcp_tunnel::TcpTunnel;

#[derive(Clone)]
pub struct TcpClients {
    active: ActiveClients,
    pub use_special_lan: bool,
}

#[derive(Clone)]
pub struct ActiveClients {
    active: Arc<RwLock<HashMap<(SocketAddr, SocketAddr), NewClient>>>,
}

impl ActiveClients {
    async fn add_new(&self, client: NewClient) -> Option<Dropper> {
        let key = (client.peer_addr, client.connect_addr);
        let mut lock = self.active.write().await;

        match lock.entry(key) {
            Entry::Occupied(_) => None,
            Entry::Vacant(v) => {
                v.insert(client);

                Some(Dropper {
                    key,
                    inner: self.clone(),
                })
            }
        }
    }

    pub async fn len(&self) -> usize {
        self.active.read().await.len()
    }

    pub async fn get_clients(&self) -> Vec<NewClient> {
        let lock = self.active.read().await;
        lock.values().map(|v| v.clone()).collect()
    }

    pub async fn client_count_by_agent_tunnel(&self, tunnel: &AgentTunnel) -> usize {
        let tunnel_ip = tunnel.to_tunnel_ip();
        let ip = MatchIp::new(tunnel_ip);

        let lock = self.active.read().await;
        lock.values().filter(|v| {
            ip.matches(v.connect_addr.ip()) && tunnel.port.contains(v.connect_addr.port())
        }).count()
    }
}

impl Default for ActiveClients {
    fn default() -> Self {
        ActiveClients {
            active: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl TcpClients {
    pub fn new() -> Self {
        TcpClients {
            active: ActiveClients::default(),
            use_special_lan: true
        }
    }

    pub fn active_clients(&self) -> ActiveClients {
        self.active.clone()
    }

    pub async fn connect(&self, new_client: NewClient) -> std::io::Result<Option<TcpClient>> {
        let claim_instructions = new_client.claim_instructions.clone();

        let Some(dropper) = self.active.add_new(new_client).await else { return Ok(None) };
        let stream = TcpTunnel::new(claim_instructions).connect().await?;

        Ok(Some(TcpClient {
            stream,
            dropper,
        }))
    }
}

pub struct TcpClient {
    stream: TcpStream,
    dropper: Dropper,
}

pub struct TcpClientWrite {
    stream: OwnedWriteHalf,
    dropper: Arc<Dropper>,
}

pub struct TcpClientRead {
    stream: OwnedReadHalf,
    dropper: Arc<Dropper>,
}

struct Dropper {
    key: (SocketAddr, SocketAddr),
    inner: ActiveClients,
}

impl TcpClient {
    pub fn into_split(self) -> (TcpClientRead, TcpClientWrite) {
        let (read, write) = self.stream.into_split();
        let dropper = Arc::new(self.dropper);

        (
            TcpClientRead {
                stream: read,
                dropper: dropper.clone(),
            },
            TcpClientWrite {
                stream: write,
                dropper: dropper.clone(),
            }
        )
    }
}

impl AsyncWrite for TcpClient {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}


impl AsyncWrite for TcpClientWrite {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl AsyncRead for TcpClient {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncRead for TcpClientRead {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl Drop for Dropper {
    fn drop(&mut self) {
        let key = self.key;
        let inner = self.inner.active.clone();

        tokio::spawn(async move {
            let mut lock = inner.write().await;
            lock.remove(&key);
        });
    }
}