use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tracing::Instrument;

use agent_common::{ClaimInstructions, NewClient};

use crate::events::SetupFailReason;
use crate::lan_address::LanAddress;

#[derive(Clone)]
pub struct TcpConnection {
    pub client_token: Vec<u8>,
    pub peer_address: SocketAddr,
    pub tunnel_address: SocketAddrV4,
    pub span: tracing::Span,
}

const RESP_LEN: usize = 8;

impl TcpConnection {
    pub async fn spawn(client: NewClient, host_addr: SocketAddr) -> Result<ActiveTcpConnection, SetupFailReason> {
        match client.claim_instructions {
            ClaimInstructions::Tcp { address, token } => {
                let span = tracing::info_span!("tcp client",
                    tunnel_address = %address,
                    local_address = %host_addr,
                );

                let conn_span = span.clone();
                async {
                    tracing::info!("new client");

                    let tcp_conn = TcpConnection {
                        client_token: token,
                        peer_address: SocketAddr::V4(client.peer_addr),
                        tunnel_address: address,
                        span: conn_span,
                    };

                    let ready = match tcp_conn.establish().await {
                        Ok(v) => v,
                        Err(error) => {
                            tracing::error!(?error, "failed to establish connection to tunnel server");
                            return Err(SetupFailReason::TunnelServerNoConnect(error));
                        }
                    };

                    let active = match ready
                        .connect_to_host(host_addr, Arc::new(Stats::default()))
                        .await
                    {
                        Ok(v) => v,
                        Err(error) => {
                            tracing::error!(?error, "failed to connect to local service");
                            return Err(SetupFailReason::LocalServerNoConnect(error));
                        }
                    };

                    tracing::info!(stats = ?active.stats, "connection setup");

                    Ok(active)
                }.instrument(span).await
            }
        }
    }

    pub async fn establish(self) -> std::io::Result<ReadyTcpConnection> {
        let span = self.span.clone();

        async {
            let mut stream = match TcpStream::connect(self.tunnel_address).await {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to connect to tunnel server");
                    return Err(error);
                }
            };

            if let Err(error) = stream.set_nodelay(true) {
                tracing::warn!(?error, "failed to set TCP no delay");
            }

            if let Err(error) = stream.write_all(&self.client_token).await {
                tracing::error!(?error, "failed to send tcp claim token");
                return Err(error);
            }

            let mut resp = [0u8; RESP_LEN];
            let size = match stream.read_exact(&mut resp).await {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to complete TCP new client handshake");
                    return Err(error);
                }
            };

            if size != RESP_LEN {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "did not get valid response",
                ));
            }

            Ok(ReadyTcpConnection {
                connection: stream,
                peer_address: self.peer_address,
                span,
            })
        }.instrument(self.span).await
    }
}

#[derive(Clone)]
pub struct ReadyTcpConnection {
    connection: TcpStream,
    peer_address: SocketAddr,
    span: tracing::Span,
}

impl ReadyTcpConnection {
    pub async fn connect_to_host(
        self,
        host_addr: SocketAddr,
        stats: Arc<Stats>,
    ) -> std::io::Result<ActiveTcpConnection> {
        async {
            let conn = match LanAddress::tcp_socket(true, self.peer_address, host_addr).await {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to connect to local server (is your server running?)");
                    return Err(error);
                }
            };

            if let Err(error) = conn.set_nodelay(true) {
                tracing::warn!(?error, "failed to set no delay");
            }

            let (host_rx, host_tx) = conn.into_split();
            let (tunnel_rx, tunnel_tx) = self.connection.into_split();

            Ok(ActiveTcpConnection {
                stats: stats.clone(),
                host_to_tunnel: tokio::spawn(
                    pipe(host_rx, tunnel_tx, stats.clone(), false)
                        .instrument(tracing::info_span!("local to tunnel"))
                ),
                tunnel_to_host: tokio::spawn(
                    pipe(tunnel_rx, host_tx, stats.clone(), true)
                        .instrument(tracing::info_span!("tunnel to local"))
                ),
            })
        }.instrument(self.span).await
    }
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct ActiveTcpConnection {
    pub stats: Arc<Stats>,
    host_to_tunnel: JoinHandle<std::io::Result<()>>,
    tunnel_to_host: JoinHandle<std::io::Result<()>>,
}

impl ActiveTcpConnection {
    pub async fn wait(self) {
        self.host_to_tunnel.await;
        self.tunnel_to_host.await;
    }
}

#[derive(Default, Debug, Clone)]
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
            }.fetch_add(received, Ordering::SeqCst);

            to.write_all(&buffer[..received]).await.map_err(|error| {
                tracing::error!(?error, "failed to write data");
                error
            })?;
        }

        Ok(())
    }.await;

    stats.running.fetch_sub(1, Ordering::SeqCst);
    r
}
