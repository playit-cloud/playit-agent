use std::net::{SocketAddr};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::net::TcpStream;
use tracing::Instrument;


use crate::api::api::PortType;

use crate::network::address_lookup::AddressLookup;
use crate::network::tcp_clients::TcpClients;
use crate::network::tcp_pipe::pipe;
use crate::network::udp_clients::UdpClients;
use crate::tunnel::setup::SetupError;
use crate::tunnel::simple_tunnel::SimpleTunnel;
use crate::tunnel::udp_tunnel::UdpTunnelRx;

pub struct TunnelRunner<L: AddressLookup> {
    lookup: Arc<L>,
    tunnel: SimpleTunnel,
    udp_clients: UdpClients<Arc<L>>,
    tcp_clients: TcpClients,
    keep_running: Arc<AtomicBool>,
}

impl<L: AddressLookup + Sync + Send> TunnelRunner<L> where L::Value: Into<SocketAddr> {
    pub async fn new(secret_key: String, lookup: Arc<L>) -> Result<Self, SetupError> {
        let tunnel = SimpleTunnel::setup(secret_key).await?;
        let udp_clients = UdpClients::new(tunnel.udp_tunnel(), lookup.clone());

        Ok(TunnelRunner {
            lookup,
            tunnel,
            udp_clients,
            tcp_clients: TcpClients::new(),
            keep_running: Arc::new(AtomicBool::new(true)),
        })
    }

    pub fn set_use_special_lan(&mut self, set_use: bool) {
        self.tcp_clients.use_special_lan = set_use;
        self.udp_clients.use_special_lan = set_use;
    }

    pub fn keep_running(&self) -> Arc<AtomicBool> {
        self.keep_running.clone()
    }

    pub async fn run(self) {
        let mut tunnel = self.tunnel;
        let udp = tunnel.udp_tunnel();

        let tunnel_run = self.keep_running.clone();
        let tunnel_task = tokio::spawn(async move {
            while tunnel_run.load(Ordering::SeqCst) {
                if let Some(new_client) = tunnel.update().await {
                    let clients = self.tcp_clients.clone();
                    let span = tracing::info_span!("tcp client", ?new_client);

                    let local_addr = match self.lookup.lookup(
                        new_client.connect_addr.ip(),
                        new_client.connect_addr.port(),
                        PortType::Tcp
                    ) {
                        Some(found) => {
                            let addr = found.value.into();
                            let port_offset = new_client.connect_addr.port() - found.from_port;
                            SocketAddr::new(addr.ip(), port_offset + addr.port())
                        },
                        None => {
                            tracing::info!("could not find local address for connection");
                            continue;
                        }
                    };

                    tokio::spawn(async move {
                        let tunnel_conn = match clients.connect(new_client.clone()).await {
                            Ok(Some(client)) => client,
                            Ok(None) => return,
                            Err(error) => {
                                tracing::error!(?error, "failed to accept new client");
                                return;
                            }
                        };

                        tracing::info!("connected to TCP tunnel");

                        let local_conn = match TcpStream::connect(local_addr).await {
                            Ok(v) => v,
                            Err(error) => {
                                tracing::error!(?error, "failed to connect to local server");
                                return;
                            }
                        };

                        let (tunnel_read, tunnel_write) = tunnel_conn.into_split();
                        let (local_read, local_write) = local_conn.into_split();

                        tokio::spawn(pipe(tunnel_read, local_write));
                        tokio::spawn(pipe(local_read, tunnel_write));
                    }.instrument(span));
                }
            }
        });

        let udp_clients = self.udp_clients;
        let udp_run = self.keep_running.clone();

        let udp_task = tokio::spawn(async move {
            let mut buffer = vec![0u8; 2048];
            let mut had_success = false;

            while udp_run.load(Ordering::SeqCst) {
                let rx = match tokio::time::timeout(Duration::from_secs(1), udp.receive_from(&mut buffer)).await {
                    Ok(Ok(v)) => v,
                    Ok(Err(error)) => {
                        if had_success {
                            tracing::error!(?error, "got error");
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                    Err(_) => continue,
                };

                had_success = true;

                match rx {
                    UdpTunnelRx::ReceivedPacket { bytes, flow } => {
                        // tracing::info!(bytes, ?flow, "got packet");
                        udp_clients.forward_packet(&flow, &buffer[..bytes]).await.unwrap();
                    }
                    UdpTunnelRx::ConfirmedConnection => {}
                }
            }
        });

        tunnel_task.await.unwrap();
        udp_task.await.unwrap();
    }
}