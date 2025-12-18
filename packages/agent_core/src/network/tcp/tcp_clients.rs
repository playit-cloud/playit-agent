use std::{net::SocketAddr, num::NonZeroU32, sync::Arc, time::Duration};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_agent_proto::control_feed::NewClient;
use playit_api_client::api::ProxyProtocol;
use serde::Serialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc::{channel, Receiver, Sender},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{
    network::{
        lan_address::LanAddress, origin_lookup::OriginLookup, proxy_protocol::ProxyProtocolHeader,
    },
    utils::now_milli,
};

use super::{
    tcp_client::{TcpClient, TcpClientStat},
    tcp_errors::tcp_errors,
    tcp_settings::TcpSettings,
};

pub struct TcpClients {
    events_tx: Sender<Event>,
    new_client_limiter: DefaultDirectRateLimiter,
    cancel: CancellationToken,
}

struct Worker {
    lookup: Arc<OriginLookup>,
    events: Receiver<Event>,
    events_tx: Sender<Event>,
    cancel: CancellationToken,
    settings: TcpSettings,

    clients: Vec<Client>,
    next_client_id: u64,
}

struct Client {
    id: u64,
    added_at: u64,
    tunnel_id: u64,
    port_offset: u16,
    source_addr: SocketAddr,
    tunnel_addr: SocketAddr,
    origin_addr: SocketAddr,
    tcp: TcpClient,
}

impl Client {
    fn details(&self) -> TcpClientDetails {
        TcpClientDetails {
            id: self.id,
            added_at: self.added_at,
            tunnel_id: self.tunnel_id,
            port_offset: self.port_offset,
            source_addr: self.source_addr,
            tunnel_addr: self.tunnel_addr,
            origin_addr: self.origin_addr,
            last_use: self.tcp.last_use(),
            bytes_written: self.tcp.bytes_written(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TcpClientDetails {
    pub id: u64,
    pub added_at: u64,
    pub tunnel_id: u64,
    pub port_offset: u16,
    pub source_addr: SocketAddr,
    pub tunnel_addr: SocketAddr,
    pub origin_addr: SocketAddr,
    pub last_use: TcpClientStat,
    pub bytes_written: TcpClientStat,
}

enum Event {
    ClearOld,
    NewClient(NewClient),
    ConnectedClient(Client),
    GetDetails(tokio::sync::oneshot::Sender<Vec<TcpClientDetails>>),
}

impl TcpClients {
    pub fn new(settings: TcpSettings, lookup: Arc<OriginLookup>) -> Self {
        let quota = unsafe {
            Quota::per_second(NonZeroU32::new_unchecked(settings.new_client_ratelimit)).allow_burst(
                NonZeroU32::new_unchecked(settings.new_client_ratelimit_burst),
            )
        };

        let (events_tx, events_rx) = channel(1024);
        let cancel = CancellationToken::new();

        tokio::spawn(
            Worker {
                next_client_id: 1,
                lookup,
                events: events_rx,
                events_tx: events_tx.clone(),
                cancel: cancel.clone(),
                settings,
                clients: Vec::with_capacity(32),
            }
            .start(),
        );

        TcpClients {
            new_client_limiter: RateLimiter::direct(quota),
            events_tx,
            cancel,
        }
    }

    pub async fn get_details(&self) -> Vec<TcpClientDetails> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.events_tx
            .send(Event::GetDetails(tx))
            .await
            .expect("TcpClients worker closed");
        rx.await.expect("TcpClients worker closed")
    }

    pub async fn handle_new_client(&self, new_client: NewClient) {
        if self.new_client_limiter.check().is_err() {
            tcp_errors().new_client_rate_limited.inc();
            return;
        }

        self.events_tx
            .send(Event::NewClient(new_client))
            .await
            .expect("TcpClients worker closed");
    }
}

impl Drop for TcpClients {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Worker {
    pub async fn start(mut self) {
        let mut next_clear = Instant::now() + Duration::from_secs(15);

        loop {
            let event = tokio::select! {
                recv_opt = self.events.recv() => recv_opt.unwrap(),
                _ = tokio::time::sleep_until(next_clear) => {
                    next_clear = Instant::now() + Duration::from_secs(15);
                    Event::ClearOld
                },
                _ = self.cancel.cancelled() => {
                    tracing::info!("TcpClients worker closed via cancel");
                    break
                },
            };

            match event {
                Event::NewClient(details) => {
                    let client_id = self.next_client_id;
                    self.next_client_id = client_id + 1;

                    tracing::info!(?details, id = client_id, "New TCP Client");

                    let Some(found) = self.lookup.lookup(details.tunnel_id, true).await else {
                        tracing::info!(
                            tunnel_id = details.tunnel_id,
                            "Could not find tunnel for new client"
                        );
                        tcp_errors().new_client_origin_not_found.inc();
                        continue;
                    };

                    let proxy_header = match (details.peer_addr, details.connect_addr) {
                        (SocketAddr::V4(peer), SocketAddr::V4(tunn)) => {
                            ProxyProtocolHeader::AfInet {
                                client_ip: *peer.ip(),
                                proxy_ip: *tunn.ip(),
                                client_port: peer.port(),
                                proxy_port: tunn.port(),
                            }
                        }
                        (SocketAddr::V6(peer), SocketAddr::V6(tunn)) => {
                            ProxyProtocolHeader::AfInet6 {
                                client_ip: *peer.ip(),
                                proxy_ip: *tunn.ip(),
                                client_port: peer.port(),
                                proxy_port: tunn.port(),
                            }
                        }
                        _ => {
                            tracing::error!("Tunnel server provide miss match protol versions for peer and connect addr");
                            tcp_errors().invalid_proto_match.inc();
                            continue;
                        }
                    };

                    let Some(origin_addr) = found.resolve_local(details.port_offset) else {
                        tracing::error!(
                            port_offset = details.port_offset,
                            tunnel_id = details.tunnel_id,
                            "port offset not valid for tunnel"
                        );
                        tcp_errors().new_client_invalid_port_offset.inc();
                        continue;
                    };

                    let setting_tcp_no_delay = self.settings.tcp_no_delay;

                    let event_tx = self.events_tx.clone();
                    tokio::spawn(async move {
                        /* connect to tunnel server */

                        let conn_res = tokio::time::timeout(
                            Duration::from_secs(8),
                            TcpStream::connect(details.claim_instructions.address),
                        )
                        .await;

                        let mut tunn_stream = match conn_res {
                            Ok(Ok(stream)) => stream,
                            Err(_) => {
                                tracing::error!("timeout connecting to claim address");
                                tcp_errors().new_client_claim_connect_timeout.inc();
                                return;
                            }
                            Ok(Err(error)) => {
                                tracing::error!(?error, "io error connecting to claim address");
                                tcp_errors().new_client_claim_connect_error.inc();
                                return;
                            }
                        };

                        if let Err(error) = tunn_stream.set_nodelay(setting_tcp_no_delay) {
                            tracing::error!(
                                ?error,
                                "failed to set tunn tcp no delay, value: {}",
                                setting_tcp_no_delay
                            );
                            tcp_errors().new_client_set_tunnel_no_delay_error.inc();
                        }

                        /* send token to tunnel server to claim client */

                        let send_res = tokio::time::timeout(
                            Duration::from_secs(8),
                            tunn_stream.write_all(&details.claim_instructions.token),
                        )
                        .await;
                        match send_res {
                            Ok(Ok(_)) => {}
                            Err(_) => {
                                tracing::error!("timeout sending claim token");
                                tcp_errors().new_client_send_claim_timeout.inc();
                                return;
                            }
                            Ok(Err(error)) => {
                                tracing::error!(
                                    ?error,
                                    "io error sending claim instruction to claim address"
                                );
                                tcp_errors().new_client_send_claim_error.inc();
                                return;
                            }
                        }

                        let mut expect_buffer = [0u8; 8];
                        let confirm_res = tokio::time::timeout(
                            Duration::from_secs(4),
                            tunn_stream.read_exact(&mut expect_buffer[..]),
                        )
                        .await;
                        match confirm_res {
                            Ok(Ok(_)) => {}
                            Err(_) => {
                                tracing::error!("timeout reading claim token response");
                                tcp_errors().new_client_claim_expect_timeout.inc();
                                return;
                            }
                            Ok(Err(error)) => {
                                tracing::error!(?error, "io error reading claim response");
                                tcp_errors().new_client_claim_expect_error.inc();
                                return;
                            }
                        }

                        /* connect to origin */

                        let connect_res = tokio::time::timeout(
                            Duration::from_secs(2),
                            LanAddress::tcp_socket(true, details.peer_addr, origin_addr),
                        )
                        .await;

                        let mut origin_stream = match connect_res {
                            Ok(Ok(stream)) => stream,
                            Ok(Err(error)) => {
                                tracing::error!(
                                    ?error,
                                    "io error failed to connect to origin: {:?}",
                                    origin_addr
                                );
                                tcp_errors().new_client_origin_connect_error.inc();
                                return;
                            }
                            Err(_) => {
                                tracing::error!("timeout connecting to origin: {}", origin_addr);
                                tcp_errors().new_client_origin_connect_timeout.inc();
                                return;
                            }
                        };

                        if let Err(error) = origin_stream.set_nodelay(true) {
                            tracing::error!(?error, "failed to set origin tcp no delay");
                            tcp_errors().new_client_set_origin_no_delay_error.inc();
                        }

                        let proxy_write_res = match found.proxy_protocol {
                            Some(ProxyProtocol::ProxyProtocolV1) => {
                                tokio::time::timeout(
                                    Duration::from_secs(2),
                                    proxy_header.write_v1_tcp(&mut origin_stream),
                                )
                                .await
                            }
                            Some(ProxyProtocol::ProxyProtocolV2) => {
                                tokio::time::timeout(
                                    Duration::from_secs(2),
                                    proxy_header.write_v2_tcp(&mut origin_stream),
                                )
                                .await
                            }
                            None => Ok(Ok(())),
                        };

                        match proxy_write_res {
                            Ok(Ok(_)) => {}
                            Err(_) => {
                                tracing::error!("timeout sending proxy protocol header");
                                tcp_errors().new_client_write_proxy_proto_timeout.inc();
                                return;
                            }
                            Ok(Err(error)) => {
                                tracing::error!(?error, "failed to write proxy protocol header");
                                tcp_errors().new_client_write_proxy_proto_error.inc();
                                return;
                            }
                        }

                        let tcp_client = TcpClient::create(tunn_stream, origin_stream).await;
                        let _ = event_tx
                            .send(Event::ConnectedClient(Client {
                                id: client_id,
                                added_at: now_milli(),
                                tunnel_id: details.tunnel_id,
                                port_offset: details.port_offset,
                                source_addr: details.peer_addr,
                                tunnel_addr: details.connect_addr,
                                origin_addr,
                                tcp: tcp_client,
                            }))
                            .await;
                    });
                }
                Event::GetDetails(resp) => {
                    let _ = resp.send(self.clients.iter().map(Client::details).collect());
                }
                Event::ConnectedClient(client) => {
                    self.clients.push(client);
                }
                Event::ClearOld => {
                    let now = now_milli();
                    self.clients.retain(|client| {
                        let last_use = client.tcp.last_use();

                        let since_tunn = now.max(last_use.tunn_to_origin) - last_use.tunn_to_origin;
                        let since_orig = now.max(last_use.origin_to_tunn) - last_use.origin_to_tunn;

                        if 90_000 < since_tunn && 30_000 < since_orig {
                            tracing::info!(id = client.id, "clear old: 90s since tunnel data");
                            return false;
                        }

                        if 90_000 < since_orig && 30_000 < since_tunn {
                            tracing::info!(id = client.id, "clear old: 90s since origin data");
                            return false;
                        }

                        if 60_000 < since_tunn && 60_000 < since_orig {
                            tracing::info!(id = client.id, "clear old: 60s since any data");
                            return false;
                        }

                        true
                    });
                }
            }
        }
    }
}
