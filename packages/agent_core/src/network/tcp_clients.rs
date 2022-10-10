use std::collections::{HashMap, HashSet};
use std::io::Error;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::RwLock;

use playit_agent_proto::control_feed::{ClaimInstructions, NewClient};

use crate::tunnel::tcp_tunnel::TcpTunnel;

#[derive(Clone)]
pub struct TcpClients {
    inner: Arc<RwLock<Inner>>,
}

struct Inner {
    active: HashMap<(SocketAddr, SocketAddr), NewClient>,
}

impl TcpClients {
    pub fn new() -> Self {
        TcpClients {
            inner: Arc::new(RwLock::new(Inner {
                active: HashMap::new(),
            }))
        }
    }

    pub async fn connect(&self, new_client: NewClient) -> std::io::Result<Option<TcpClient>> {
        let key = (new_client.peer_addr, new_client.connect_addr);

        let claim_instructions = new_client.claim_instructions.clone();

        {
            let mut lock = self.inner.write().await;
            if lock.active.contains_key(&key) {
                return Ok(None);
            }
            lock.active.insert(key, new_client);
        }

        /* create dropper before connect so on failure / timeout we clean up active map */
        let dropper = Dropper {
            key,
            inner: self.inner.clone(),
        };

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
    inner: Arc<RwLock<Inner>>,
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
        let inner = self.inner.clone();

        tokio::spawn(async move {
            let mut lock = inner.write().await;
            lock.active.remove(&key);
        });
    }
}