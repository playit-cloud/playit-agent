use std::net::{SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

pub struct TcpConnection {
    pub client_token: Vec<u8>,
    pub tunnel_address: SocketAddrV4,
}

const RESP_LEN: usize = 8;

impl TcpConnection {
    pub async fn establish(self) -> std::io::Result<ReadyTcpConnection> {
        let mut stream = TcpStream::connect(self.tunnel_address).await?;
        stream.set_nodelay(true)?;
        stream.write_all(&self.client_token).await?;

        let mut resp = [0u8; RESP_LEN];
        let size = stream.read_exact(&mut resp).await?;
        if size != RESP_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "did not get valid resposne",
            ));
        }

        Ok(ReadyTcpConnection { connection: stream })
    }
}

pub struct ReadyTcpConnection {
    connection: TcpStream,
}

impl ReadyTcpConnection {
    pub async fn connect_to_host(
        self,
        host_addr: SocketAddr,
        stats: Arc<Stats>,
    ) -> std::io::Result<ActiveTcpConnection> {
        let conn = TcpStream::connect(host_addr).await?;
        conn.set_nodelay(true)?;

        let (host_rx, host_tx) = conn.into_split();
        let (tunnel_rx, tunnel_tx) = self.connection.into_split();

        Ok(ActiveTcpConnection {
            stats: stats.clone(),
            host_to_tunnel: tokio::spawn(pipe(host_rx, tunnel_tx, stats.clone(), false)),
            tunnel_to_host: tokio::spawn(pipe(tunnel_rx, host_tx, stats.clone(), true)),
        })
    }
}

pub struct ActiveTcpConnection {
    pub stats: Arc<Stats>,
    host_to_tunnel: JoinHandle<std::io::Result<()>>,
    tunnel_to_host: JoinHandle<std::io::Result<()>>,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub running: AtomicUsize,
    pub from_tunnel: AtomicUsize,
    pub to_tunnel: AtomicUsize,
}

async fn pipe(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    stats: Arc<Stats>,
    from_tunnel: bool,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    buffer.resize(2048, 0u8);

    stats.running.fetch_add(1, Ordering::SeqCst);

    let r = async {
        loop {
            tokio::task::yield_now().await;

            let received = from.read(&mut buffer[..]).await.map_err(|error| {
                tracing::error!(?error, "failed to read data");
                error
            })?;

            if received == 0 {
                tracing::info!("pipe ended due to EOF");
                break;
            }

            if from_tunnel {
                &stats.from_tunnel
            } else {
                &stats.to_tunnel
            }
            .fetch_add(received, Ordering::SeqCst);

            to.write_all(&buffer[..received]).await.map_err(|error| {
                tracing::error!(?error, "failed to write data");
                error
            })?;
        }

        Ok(())
    }
    .await;

    stats.running.fetch_sub(1, Ordering::SeqCst);

    r
}
