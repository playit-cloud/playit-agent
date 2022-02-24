use std::net::{IpAddr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use byteorder::{BigEndian, ByteOrder};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::channel;
use tokio::sync::RwLock;

use agent_common::{ClaimLease, ClaimProto, NewClient, Proto};
use agent_common::agent_config::AgentConfig;
use agent_common::udp::{RedirectFlowFooter, UDP_CHANNEL_ESTABLISH_ID};

use crate::agent_config::{AgentConfigStatus, ManagedAgentConfig};
use crate::api_client::ApiClient;
use crate::events::{PlayitEventDetails, PlayitEvents};
use crate::now_milli;
use crate::tcp_client::{Stats, TcpConnection};
use crate::tunnel_client::TunnelClient;
use crate::udp_client::UdpClients;

#[derive(Clone)]
pub struct Application {
    pub events: PlayitEvents,
    pub agent_config: ManagedAgentConfig,
    pub render_state: Arc<RwLock<AgentState>>,
}

pub enum AgentState {
    PreparingConfig(Arc<RwLock<AgentConfigStatus>>),
    WaitingForTunnels { error: bool },
    ConnectingToTunnelServer,
    FailedToConnect,
    Running(RunningState),
}

pub struct RunningState {
    pub account_id: u64,
    pub events: PlayitEvents,
    pub agent_config: ManagedAgentConfig,
    pub claim_failed: bool,
}

impl Application {
    pub async fn start(self) {
        let udp_tunnel = match UdpSocket::bind(SocketAddrV4::new(0.into(), 0)).await {
            Ok(v) => Arc::new(v),
            Err(error) => {
                tracing::error!(?error, "failed to setup UDP socket");
                return;
            }
        };

        if self.run_setup().await {
            return;
        }

        self.set_state(AgentState::ConnectingToTunnelServer).await;

        let (
            new_client_tx,
            mut new_client_rx
        ) = channel::<NewClient>(1024);

        let client_ids = Arc::new(AtomicU64::new(1));

        let handle_tcp_clients = {
            let this = self.clone();
            let client_ids = client_ids.clone();

            tokio::spawn(async move {
                while let Some(client) = new_client_rx.recv().await {
                    let found = this.agent_config.with_config(|config| {
                        config.find_local_addr(client.connect_addr, Proto::Tcp)
                    }).await;

                    let host_addr = match found {
                        Some((_local_bind, host_addr)) => host_addr,
                        None => {
                            tracing::warn!(?client, "got TCP client without tunnel");
                            continue;
                        }
                    };

                    let client_id = client_ids.fetch_add(1, Ordering::SeqCst);

                    this.events.add_event(PlayitEventDetails::ClientAccepted {
                        client_id,
                        proto: Proto::Tcp,
                        tunnel_addr: SocketAddr::V4(client.connect_addr),
                        peer_addr: SocketAddr::V4(client.peer_addr),
                        host_addr,
                    }).await;

                    let this = this.clone();
                    tokio::spawn(async move {
                        let pipe = match TcpConnection::spawn(client, host_addr).await {
                            Ok(pipe) => pipe,
                            Err(reason) => {
                                this.events.add_event(PlayitEventDetails::NewClientSetupFailed {
                                    client_id,
                                    reason,
                                }).await;
                                return;
                            }
                        };

                        this.events.add_event(PlayitEventDetails::ClientConnected { client_id }).await;

                        pipe.wait().await;
                        this.events.add_event(PlayitEventDetails::ClientDisconnected { client_id }).await;
                    });
                }
            })
        };

        let tunnel_client = {
            let api_client = self.agent_config.with_config(|config| {
                let api_url = config.get_api_url();
                ApiClient::new(api_url, Some(config.secret_key.clone()))
            }).await;

            match TunnelClient::new(api_client, new_client_tx).await {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to setup tunnel client");
                    return;
                }
            }
        };

        /* register program with tunnel server */
        loop {
            let registered = match tunnel_client.register().await {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to register program");
                    self.set_state(AgentState::FailedToConnect).await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            tracing::info!(details = ?registered, "program registered");
            self.set_state(AgentState::Running(RunningState {
                account_id: registered.account_id,
                events: self.events.clone(),
                agent_config: self.agent_config.clone(),
                claim_failed: false,
            })).await;

            break;
        }

        /* sets up the tunnel servers for our use */
        self.claim_port_leases(&tunnel_client).await;

        /* setup UDP channel */
        let udp_channel = loop {
            match tunnel_client.setup_udp_channel().await {
                Ok(v) => break Arc::new(RwLock::new(v)),
                Err(error) => {
                    tracing::error!(?error, "failed to setup UDP channel");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
            }
        };

        let mut last_udp_keep_alive_response = Arc::new(AtomicU64::new(now_milli()));

        let keep_udp_alive = {
            let udp_tunnel = udp_tunnel.clone();
            let udp_channel = udp_channel.clone();
            let last_udp_keep_alive_response = last_udp_keep_alive_response.clone();
            let tunnel_client = tunnel_client.clone();

            tokio::spawn(async move {
                loop {
                    let now = now_milli();
                    if last_udp_keep_alive_response.load(Ordering::SeqCst) + 15_000 < now {
                        let setup = match tunnel_client.setup_udp_channel().await {
                            Ok(v) => v,
                            Err(error) => {
                                tracing::error!(?error, "failed to setup udp channel");
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                continue;
                            }
                        };

                        *udp_channel.write().await = setup;
                        last_udp_keep_alive_response.store(now_milli(), Ordering::SeqCst);
                    }

                    let send_res = {
                        let channel = udp_channel.read().await;
                        udp_tunnel.send_to(&channel.token, channel.tunnel_addr).await
                    };

                    if let Err(error) = send_res {
                        tracing::error!("failed to send UDP register message: {:?}", error);
                    }

                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            })
        };

        let handle_udp_packets = {
            let udp_tunnel = udp_tunnel.clone();
            let udp_channel = udp_channel.clone();
            let last_udp_keep_alive_response = last_udp_keep_alive_response.clone();
            let tunnel_udp_addr: IpAddr = "147.185.221.2".parse().unwrap();
            let this = self.clone();

            let mut udp_clients = UdpClients::new(
                udp_tunnel.clone(),
                udp_channel.clone(),
                client_ids.clone(),
                self.events.clone(),
            );

            tokio::spawn(async move {
                let mut buffer = vec![0; 2048];

                loop {
                    let (bytes, from) = match udp_tunnel.recv_from(&mut buffer).await {
                        Ok(v) => v,
                        Err(error) => {
                            tracing::error!(?error, "failed reading from UDP channel");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    };

                    if from.ip() != tunnel_udp_addr {
                        continue;
                    }

                    let packet = &buffer[..bytes];
                    let update_keep_alive = this.handle_udp_packet(&mut udp_clients, packet).await;

                    if update_keep_alive {
                        last_udp_keep_alive_response.store(now_milli(), Ordering::SeqCst);
                    }
                }
            })
        };

        /* refresh configs periodically */
        let config_update_task = {
            let config = self.agent_config.clone();
            let tunnel_client = tunnel_client.clone();
            let this = self.clone();

            tokio::spawn(async move {
                loop {
                    let sleep_sec = match config.load_latest().await {
                        Ok(true) => {
                            this.claim_port_leases(&tunnel_client).await;
                            5
                        }
                        Ok(false) => 10,
                        Err(_) => 20,
                    };

                    tokio::time::sleep(Duration::from_secs(sleep_sec)).await;
                }
            })
        };

        /* send keep alive packets */
        let keep_alive_task = {
            let tunnel_client = tunnel_client.clone();
            let this = self.clone();

            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    if !tunnel_client.keep_alive().await.unwrap_or(false) {
                        if let Err(error) = tunnel_client.register().await {
                            tracing::error!(?error, "failed to register");
                        }

                        /* we may have lost keep alive due to session reset so resend leases */
                        this.claim_port_leases(&tunnel_client).await;
                    }
                }
            })
        };

        config_update_task.await;
        keep_alive_task.await;
        handle_tcp_clients.await;
        keep_udp_alive.await;
        handle_udp_packets.await;
    }

    async fn handle_udp_packet(&self, udp_clients: &mut UdpClients, buffer: &[u8]) -> bool {
        let bytes = buffer.len();

        if bytes < 8 {
            tracing::warn!(bytes, "got invalid tiny UDP channel message");
            return false;
        }

        let id = BigEndian::read_u64(&buffer[bytes - 8..bytes]);
        if id == UDP_CHANNEL_ESTABLISH_ID {
            tracing::info!("got UDP establish response");
            return true;
        }

        let flow = match RedirectFlowFooter::from_tail(&buffer[..bytes]) {
            Some(v) => v,
            None => {
                tracing::error!(id, bytes, "got channel message with unknown id");
                return false;
            }
        };

        let payload = &buffer[..bytes - RedirectFlowFooter::len()];

        let agent_config = self.agent_config.clone();

        udp_clients
            .forward_packet(flow, payload, move |addr| {
                agent_config.into_local_lookup(addr, Proto::Udp)
            }).await;

        false
    }

    async fn claim_port_leases(&self, client: &TunnelClient) {
        let claims = self.agent_config.with_config(|config| {
            let mut claims = Vec::new();

            for mapping in &config.mappings {
                let ip = match mapping.tunnel_ip {
                    IpAddr::V4(ip) => ip,
                    _ => {
                        tracing::error!("IPv6 not supported");
                        continue;
                    }
                };

                claims.push(ClaimLease {
                    ip,
                    from_port: mapping.tunnel_from_port,
                    to_port: mapping.tunnel_to_port
                        .unwrap_or(mapping.tunnel_from_port + 1),
                    proto: mapping.proto,
                });
            }

            claims
        }).await;

        let mut has_error = false;

        for claim in claims {
            if let Err(error) = client.claim_lease(claim.clone()).await {
                tracing::error!(?error, ?claim, "failed to claim lease");
                has_error = true;
            }
        }

        self.update_running(|running| {
            running.claim_failed = has_error;
        }).await;
    }

    async fn run_setup(&self) -> bool {
        if let Err(error) = self.agent_config.prepare_config().await {
            tracing::error!(?error, "got error preparing config");
            return true;
        }

        tracing::info!("agent config is prepared, checking for tunnels");

        if self.agent_config.with_config(|c| c.mappings.len()).await == 0 {
            tracing::info!("no tunnels, refresh till we get something");
            self.set_state(AgentState::WaitingForTunnels { error: false }).await;

            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let error = self.agent_config.load_latest().await.is_err();

                let tunnel_count = self.agent_config.with_config(|c| c.mappings.len()).await;
                if tunnel_count > 0 {
                    self.events.add_event(PlayitEventDetails::AgentConfigUpdated).await;
                    break;
                }

                self.set_state(AgentState::WaitingForTunnels { error }).await;
            }
        }

        false
    }

    async fn set_state(&self, state: AgentState) {
        *self.render_state.write().await = state;
    }

    async fn update_running<F: Fn(&mut RunningState)>(&self, handle: F) -> bool {
        let mut guard = self.render_state.write().await;
        match &mut *guard {
            AgentState::Running(running) => {
                handle(running);
                true
            }
            _ => false
        }
    }
}