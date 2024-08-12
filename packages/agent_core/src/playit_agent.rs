use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::Instrument;

use crate::agent_control::{AuthApi, DualStackUdpSocket};
use crate::api::api::PortType;
use crate::network::address_lookup::AddressLookup;
use crate::network::lan_address::LanAddress;
use crate::network::tcp_clients::TcpClients;
use crate::network::tcp_pipe::pipe;
use crate::network::udp_clients::UdpClients;
use crate::agent_control::errors::SetupError;
use crate::agent_control::maintained_control::MaintainedControl;
use crate::agent_control::udp_channel::{UdpChannel, UdpTunnelRx};
use crate::utils::now_milli;

pub struct PlayitAgent<L: AddressLookup> {
    lookup: Arc<L>,
    control: MaintainedControl<DualStackUdpSocket, AuthApi>,
    udp_clients: UdpClients<Arc<L>, DualStackUdpSocket>,
    tcp_clients: TcpClients,
    keep_running: Arc<AtomicBool>,
}

impl<L: AddressLookup + Sync + Send> PlayitAgent<L> where L::Value: Into<SocketAddr> {
    pub async fn new(api_url: String, secret_key: String, lookup: Arc<L>) -> Result<Self, SetupError> {
        let io = DualStackUdpSocket::new().await?;
        let auth = AuthApi {
            api_url,
            secret_key,
        };

        let udp = UdpChannel::new(DualStackUdpSocket::new().await?);
        let tunnel = MaintainedControl::setup(io, auth, Some(udp)).await?;
        let udp_clients = UdpClients::new(tunnel.udp_channel().unwrap(), lookup.clone());

        Ok(PlayitAgent {
            lookup,
            control: tunnel,
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
        let mut tunnel = self.control;
        let udp = tunnel.udp_channel().unwrap();

        let tunnel_run = self.keep_running.clone();

        let tunnel_task = tokio::spawn(async move {
            let mut last_control_update = now_milli();

            while tunnel_run.load(Ordering::SeqCst) {
                /* refresh control address every half minute */
                {
                    let now = now_milli();
                    if 30_000 < now_milli() - last_control_update {
                        last_control_update = now;

                        if let Err(error) = tunnel.reload_control_addr(async { DualStackUdpSocket::new().await }).await {
                            tracing::error!(?error, "failed to reload_control_addr");
                        }
                    }
                }

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
                        let peer_addr = new_client.peer_addr;

                        let tunnel_conn = match clients.connect(new_client.clone()).await {
                            Ok(Some(client)) => client,
                            Ok(None) => return,
                            Err(error) => {
                                tracing::error!(?error, "failed to accept new client");
                                return;
                            }
                        };

                        tracing::info!(%local_addr, "connected to TCP tunnel");

                        let local_conn = match LanAddress::tcp_socket(self.tcp_clients.use_special_lan, peer_addr, local_addr).await {
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

            let mut invalid_count = 0;

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
                    UdpTunnelRx::InvalidEstablishToken => {
                        invalid_count += 1;

                        if 5 <= invalid_count && invalid_count < 10 {
                            tracing::error!("have repeat invalid UDP establish tokens");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        continue;
                    }
                }

                invalid_count = 0;
            }
        });

        tunnel_task.await.unwrap();
        udp_task.await.unwrap();
    }
}