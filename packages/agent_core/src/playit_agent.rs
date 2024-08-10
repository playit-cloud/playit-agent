use std::net::SocketAddr;
use std::ops::Add;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use playit_agent_proto::control_feed::NewClient;
use playit_agent_proto::control_messages::ExperimentResultEntry;
use tokio::sync::mpsc::Receiver;
use tokio::sync::RwLock;
use tracing::Instrument;

use crate::agent_control::{AuthApi, AuthResource, DualStackUdpSocket};
use crate::api::api::{AgentRoutingTarget, PortType};
use crate::network::address_lookup::AddressLookup;
use crate::network::lan_address::LanAddress;
use crate::network::tcp_clients::TcpClients;
use crate::network::tcp_pipe::pipe;
use crate::network::udp_clients::UdpClients;
use crate::agent_control::errors::SetupError;
use crate::agent_control::maintained_control::MaintainedControl;
use crate::agent_control::udp_channel::{UdpChannel, UdpTunnelRx};
use crate::ping_experiments::PingExperiments;
use crate::utils::now_milli;

pub struct PlayitAgent<L: AddressLookup> {
    lookup: Arc<L>,
    control: MaintainedControl<DualStackUdpSocket, AuthApi>,
    udp_clients: UdpClients<Arc<L>, DualStackUdpSocket>,
    tcp_clients: TcpClients,
    keep_running: Arc<AtomicBool>,
    routing_target: Arc<RwLock<AgentRoutingTarget>>,

    ping_exp: Option<PingExperiments>,
    ping_exp_entry_rx: Option<Receiver<ExperimentResultEntry>>,
}

impl<L: AddressLookup + Sync + Send> PlayitAgent<L> where L::Value: Into<SocketAddr> {
    pub async fn new(api_url: String, secret_key: String, lookup: Arc<L>) -> Result<Self, SetupError> {
        let io = DualStackUdpSocket::new().await?;
        let auth = AuthApi {
            api_url,
            secret_key,
        };

        let udp = UdpChannel::new(DualStackUdpSocket::new().await?);

        let (routing_target, addresses) = auth.get_control_addresses().await?;
        let routing_target = Arc::new(RwLock::new(routing_target));
        let (ping_exp, ping_exp_entry_rx) = match PingExperiments::new(routing_target.clone()).await {
            Ok((exp, rx)) => (Some(exp), Some(rx)),
            Err(error) => {
                tracing::error!(?error, "failed to setup ping experiments");
                (None, None)
            }
        };

        let tunnel = MaintainedControl::setup(io, auth, addresses, Some(udp)).await?;
        let udp_clients = UdpClients::new(tunnel.udp_channel().unwrap(), lookup.clone());


        Ok(PlayitAgent {
            lookup,
            control: tunnel,
            udp_clients,
            tcp_clients: TcpClients::new(),
            keep_running: Arc::new(AtomicBool::new(true)),
            routing_target,
            ping_exp,
            ping_exp_entry_rx,
        })
    }

    pub fn ping_experiments(&self) -> Option<PingExperiments> {
        self.ping_exp.clone()
    }

    pub fn set_use_special_lan(&mut self, set_use: bool) {
        self.tcp_clients.use_special_lan = set_use;
        self.udp_clients.use_special_lan = set_use;
    }

    pub fn keep_running(&self) -> Arc<AtomicBool> {
        self.keep_running.clone()
    }

    pub async fn run(self) {
        let PlayitAgent {
            lookup,
            control: mut tunnel,
            udp_clients,
            tcp_clients,
            keep_running,
            routing_target,
            ping_exp,
            mut ping_exp_entry_rx,
        } = self;

        let udp = tunnel.udp_channel().unwrap();
        let tunnel_run = keep_running.clone();

        let tcp = TcpTask {
            clients: tcp_clients,
            lookup: lookup.clone(),
        };

        let tunnel_task = tokio::spawn(async move {
            let mut last_control_update = now_milli();

            while tunnel_run.load(Ordering::SeqCst) {
                /* refresh control address every 30s */
                {
                    let now = now_milli();
                    if 30_000 < now_milli() - last_control_update {
                        last_control_update = now;

                        match tunnel.reload_control_addr(async { DualStackUdpSocket::new().await }).await {
                            Ok(Some(target)) => {
                                let mut lock = routing_target.write().await;
                                *lock = target;
                            },
                            Ok(None) => {}
                            Err(error) => {
                                tracing::error!(?error, "failed to reload_control_addr");
                            },
                        }
                    }
                }

                if let Some(rx) = ping_exp_entry_rx.as_mut() {
                    tokio::select! {
                        new_client_opt = tunnel.update() => {
                            if let Some(new_client) = new_client_opt {
                                tcp.spawn_new_tcp_client(new_client);
                            }
                        }
                        exp_entry_opt = rx.recv() => {
                            let Some(entry) = exp_entry_opt else {
                                let _ = ping_exp_entry_rx.take();
                                continue;
                            };
                            
                            tunnel.queue_ping_experiment_entry(entry);
                        }
                    }
                } else {
                    if let Some(new_client) = tunnel.update().await {
                        tcp.spawn_new_tcp_client(new_client);
                    }
                }
            }
        });

        let udp_run = keep_running.clone();

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

struct TcpTask<L: AddressLookup> {
    clients: TcpClients,
    lookup: L,
}

impl<L: AddressLookup> TcpTask<L> where  L::Value: Into<SocketAddr> {
    fn spawn_new_tcp_client(&self, new_client: NewClient) {
        let clients = self.clients.clone();
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
                return;
            }
        };

        let use_special_lan = clients.use_special_lan;

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

            let local_conn = match LanAddress::tcp_socket(use_special_lan, peer_addr, local_addr).await {
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

