use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::Instrument;

use crate::agent_control::{AuthApi, DualStackUdpSocket};
use crate::network::proxy_protocol::ProxyProtocolHeader;
use playit_api_client::api::{PortType, ProxyProtocol};
use crate::network::address_lookup::{AddressLookup, HostOrigin};
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

impl<L: AddressLookup + Sync + Send> PlayitAgent<L> where L::Value: Into<HostOrigin> + Into<SocketAddr> {
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
                tokio::task::yield_now().await;

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
                    tracing::info!(?new_client, "New TCP Client");

                    let clients = self.tcp_clients.clone();

                    let host_origin = match self.lookup.lookup(
                        new_client.connect_addr.ip(),
                        new_client.connect_addr.port(),
                        PortType::Tcp
                    ) {
                        Some(found) => {
                            let mut origin: HostOrigin = found.value.into();
                            let port_offset = new_client.connect_addr.port() - found.from_port;
                            origin.host_addr = SocketAddr::new(origin.host_addr.ip(), port_offset + origin.host_addr.port());
                            origin
                        },
                        None => {
                            tracing::info!(
                                tunnel_addr = %new_client.connect_addr.ip(),
                                tunnel_port = new_client.connect_addr.port(),
                                "could not find local address for connection"
                            );

                            continue;
                        }
                    };

                    let span = tracing::info_span!(
                        "tcp_tunnel",
                        peer_addr = %new_client.peer_addr,
                        tunn_addr = %new_client.connect_addr,
                        %host_origin,
                        sid = new_client.tunnel_server_id,
                        did = new_client.data_center_id,
                    );

                    tokio::spawn(async move {
                        let peer_addr = new_client.peer_addr;

                        let tunnel_conn = match clients.connect(new_client.clone()).await {
                            Ok(Some(client)) => client,
                            Ok(None) => {
                                tracing::warn!("got duplciate NewClient message for connection, ignoring");
                                return;
                            },
                            Err(error) => {
                                tracing::error!(?error, "failed to accept new client");
                                return;
                            }
                        };

                        tracing::info!("connected to TCP tunnel");

                        let local_conn = match LanAddress::tcp_socket(self.tcp_clients.use_special_lan, peer_addr, host_origin.host_addr).await {
                            Ok(v) => v,
                            Err(error) => {
                                tracing::error!(?error, "failed to connect to local server");
                                return;
                            }
                        };

                        if let Ok(local_addr) = local_conn.local_addr() {
                            tracing::info!("local TCP connection bound to {}", local_addr);
                        }

                        let (tunnel_read, tunnel_write) = tunnel_conn.into_split();
                        let (local_read, mut local_write) = local_conn.into_split();

                        let tunn_to_local_span = tracing::info_span!("tunn2local");
                        let local_to_tunn_span = tracing::info_span!("local2tunn");

                        tokio::spawn(async move {
                            'write_proxy_header: {
                                let Some(protocol) = host_origin.proxy_protocol else { break 'write_proxy_header };

                                let header = match (new_client.peer_addr, new_client.connect_addr) {
                                    (SocketAddr::V4(client_addr), SocketAddr::V4(proxy_addr)) => ProxyProtocolHeader::Tcp4 {
                                        client_ip: *client_addr.ip(),
                                        proxy_ip: *proxy_addr.ip(),
                                        client_port: client_addr.port(),
                                        proxy_port: proxy_addr.port(),
                                    },
                                    (SocketAddr::V6(client_addr), SocketAddr::V6(proxy_addr)) => ProxyProtocolHeader::Tcp6 {
                                        client_ip: *client_addr.ip(),
                                        proxy_ip: *proxy_addr.ip(),
                                        client_port: client_addr.port(),
                                        proxy_port: proxy_addr.port(),
                                    },
                                    _ => {
                                        tracing::warn!("peer and connect address have different protocol version");
                                        break 'write_proxy_header;
                                    }
                                };

                                let result = match protocol {
                                    ProxyProtocol::ProxyProtocolV1 => header.write_v1(&mut local_write).await,
                                    ProxyProtocol::ProxyProtocolV2 => header.write_v2(&mut local_write).await,
                                };

                                if let Err(error) = result {
                                    tracing::error!(?error, "failed to write proxy protocol header to location connection");
                                    return Err(error);
                                }
                            }

                            pipe(tunnel_read, local_write).await
                        }.instrument(tunn_to_local_span));

                        tokio::spawn(pipe(local_read, tunnel_write).instrument(local_to_tunn_span));
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
                tokio::task::yield_now().await;

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