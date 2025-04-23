use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc::channel;
use tokio::time::Instant;
use tracing::Instrument;

use crate::agent_control::{AuthApi, DualStackUdpSocket};
use crate::network::origin_lookup::OriginLookup;
use crate::network::proxy_protocol::ProxyProtocolHeader;
use crate::network::tcp::tcp_clients::TcpClients2;
use crate::network::udp::packets::Packets;
use crate::network::udp::udp_channel::UdpChannel;
use crate::network::udp::udp_clients::UdpClients;
use playit_api_client::api::{PortType, ProxyProtocol};
use crate::network::lan_address::LanAddress;
use crate::network::tcp_clients::TcpClients;
use crate::agent_control::errors::SetupError;
use crate::agent_control::maintained_control::{MaintainedControl, TunnelControlEvent};
use crate::utils::now_milli;

pub struct PlayitAgent {
    lookup: Arc<OriginLookup>,
    control: MaintainedControl<DualStackUdpSocket, AuthApi>,

    udp_clients: UdpClients,
    udp_channel: UdpChannel,

    tcp_clients: TcpClients2,
    keep_running: Arc<AtomicBool>,
}

impl PlayitAgent {
    pub async fn new(api_url: String, secret_key: String, lookup: Arc<OriginLookup>) -> Result<Self, SetupError> {
        let io = DualStackUdpSocket::new().await?;
        let auth = AuthApi::new(api_url, secret_key);

        let tunnel = MaintainedControl::setup(io, auth).await?;

        let packets = Packets::new(1024 * 16);
        let udp_clients = UdpClients::new(lookup.clone(), packets.clone());
        let udp_channel = UdpChannel::new(packets.clone()).await.map_err(SetupError::IoError)?;

        Ok(PlayitAgent {
            lookup,
            control: tunnel,
            udp_clients,
            udp_channel,
            tcp_clients: TcpClients2::default(),
            keep_running: Arc::new(AtomicBool::new(true)),
        })
    }

    // pub fn set_use_special_lan(&mut self, set_use: bool) {
    //     self.tcp_clients.use_special_lan = set_use;
    // }

    pub fn keep_running(&self) -> Arc<AtomicBool> {
        self.keep_running.clone()
    }

    pub async fn run(self) {
        let mut tunnel = self.control;
        let tunnel_run = self.keep_running.clone();

        let (udp_session_tx, mut udp_session_rx) = channel(8);
        let udp_session_should_renew = Arc::new(AtomicBool::new(false));

        let should_renew_udp = udp_session_should_renew.clone();
        let tunnel_task = tokio::spawn(async move {
            let mut last_control_update = now_milli();

            while tunnel_run.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;

                if should_renew_udp.load(Ordering::Acquire) && tunnel.send_udp_session_auth(now_milli(), 5_000).await {
                    tracing::info!("udp channel requires auth, sent auth request");
                }

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

                match tunnel.update().await {
                    Some(TunnelControlEvent::NewClient(new_client)) => {
                        tracing::info!(?new_client, "New TCP Client");

                        let Some(origin) = self.lookup.lookup(new_client.tunnel_id, true).await else {
                            tracing::info!(
                                tunnel_addr = %new_client.connect_addr.ip(),
                                tunnel_port = new_client.connect_addr.port(),
                                "could not find local address for connection"
                            );
                            continue
                        };

                        let local_addr = origin.resolve_local(new_client.port_offset);
    
                        let span = tracing::info_span!(
                            "tcp_tunnel",
                            peer_addr = %new_client.peer_addr,
                            tunn_addr = %new_client.connect_addr,
                            %host_origin,
                            dc_id = new_client.data_center_id,
                            tunn_id = new_client.tunnel_id,
                            port_offset = new_client.port_offset,
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
                                        (SocketAddr::V4(client_addr), SocketAddr::V4(proxy_addr)) => ProxyProtocolHeader::AfInet {
                                            client_ip: *client_addr.ip(),
                                            proxy_ip: *proxy_addr.ip(),
                                            client_port: client_addr.port(),
                                            proxy_port: proxy_addr.port(),
                                        },
                                        (SocketAddr::V6(client_addr), SocketAddr::V6(proxy_addr)) => ProxyProtocolHeader::AfInet6 {
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
                                        ProxyProtocol::ProxyProtocolV1 => header.write_v1_tcp(&mut local_write).await,
                                        ProxyProtocol::ProxyProtocolV2 => header.write_v2_tcp(&mut local_write).await,
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
                    Some(TunnelControlEvent::UdpChannelDetails(udp_details)) => {
                        tracing::info!("udp session details received");
                        let _ = udp_session_tx.try_send(udp_details);
                    }
                    None => {}
                }
            }
        });

        let udp_run = self.keep_running.clone();

        let mut udp_channel = self.udp_channel;
        let mut udp_clients = self.udp_clients;

        let udp_task = tokio::spawn(async move {
            let mut next_clear = Instant::now() + Duration::from_secs(16);

            while udp_run.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;

                tokio::select! {
                    recv = udp_clients.recv_origin_packet() => {
                        let Some((flow, packet)) = udp_clients.dispatch_origin_packet(now_milli(), recv).await else { continue };
                        udp_channel.send(flow, packet).await;
                    }
                    (flow, packet) = udp_channel.recv() => {
                        udp_clients.handle_tunneled_packet(now_milli(), flow, packet).await;
                    }
                    session_opt = udp_session_rx.recv() => {
                        udp_channel.update_session(session_opt.unwrap()).await;
                    }
                    _ = tokio::time::sleep_until(next_clear) => {
                        next_clear = Instant::now() + Duration::from_secs(16);
                        udp_clients.clear_old(now_milli());
                    }
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }

                {
                    let udp_needs_renew = match udp_channel.time_since_established() {
                        Some(since) => Duration::from_secs(6) <= since,
                        None => true,
                    };
                    udp_session_should_renew.store(udp_needs_renew, Ordering::Release);
                }
            }
        }.instrument(tracing::info_span!("udp_session")));

        tunnel_task.await.unwrap();
        udp_task.await.unwrap();
    }
}
