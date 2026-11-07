use std::{
    net::SocketAddr,
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_agent_proto::control_feed::NewClient;
use playit_api_client::api::ProxyProtocol;
use serde::Serialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc::{Receiver, Sender, channel},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{
    network::{
        lan_address::LanAddress,
        origin_lookup::{OriginLookup, OriginResource, OriginTarget},
        proxy_protocol::ProxyProtocolHeader,
    },
    stats::AgentStats,
    tls::{
        CertManager,
        proxy::{ProxyContext, serve_http_redirect, serve_https},
    },
    utils::now_milli,
};

use super::{
    tcp_client::{TcpClient, TcpClientStat},
    tcp_errors::tcp_errors,
    tcp_settings::TcpSettings,
};

fn build_quota(settings: &TcpSettings) -> Quota {
    let rate = NonZeroU32::new(settings.new_client_ratelimit).unwrap_or_else(|| {
        tracing::warn!("invalid tcp new client rate limit of 0, clamping to 1");
        NonZeroU32::MIN
    });
    let burst = NonZeroU32::new(settings.new_client_ratelimit_burst).unwrap_or_else(|| {
        tracing::warn!("invalid tcp new client burst of 0, clamping to 1");
        NonZeroU32::MIN
    });

    Quota::per_second(rate).allow_burst(burst)
}

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
    stats: AgentStats,

    clients: Vec<Client>,
    /// Connections served by the TLS-terminating proxy, which aren't in `clients`.
    terminated_active: Arc<AtomicU32>,
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
    pub fn new(
        settings: TcpSettings,
        lookup: Arc<OriginLookup>,
        stats: AgentStats,
        cancel: CancellationToken,
    ) -> Self {
        let quota = build_quota(&settings);
        let (events_tx, events_rx) = channel(1024);

        tokio::spawn(
            Worker {
                next_client_id: 1,
                lookup,
                events: events_rx,
                events_tx: events_tx.clone(),
                cancel: cancel.child_token(),
                settings,
                stats,
                clients: Vec::with_capacity(32),
                terminated_active: Arc::new(AtomicU32::new(0)),
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
    fn update_tcp_count(&self) {
        self.stats
            .set_tcp(self.clients.len() as u32 + self.terminated_active.load(Ordering::Relaxed));
    }

    pub async fn start(mut self) {
        let mut next_clear = Instant::now() + Duration::from_secs(15);

        loop {
            let event = tokio::select! {
                recv_opt = self.events.recv() => {
                    let Some(event) = recv_opt else {
                        tracing::debug!("TcpClients worker closed because event channel closed");
                        break;
                    };
                    event
                },
                _ = tokio::time::sleep_until(next_clear) => {
                    next_clear = Instant::now() + Duration::from_secs(15);
                    Event::ClearOld
                },
                _ = self.cancel.cancelled() => {
                    tracing::debug!("TcpClients worker closed via cancel");
                    break
                },
            };

            match event {
                Event::NewClient(details) => {
                    let client_id = self.next_client_id;
                    self.next_client_id = client_id + 1;

                    tracing::info!(?details, id = client_id, "New TCP Client");

                    let Some(found) = self.lookup.lookup(details.tunnel_id, true).await else {
                        tracing::debug!(
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
                            tracing::error!(
                                "Tunnel server provide miss match protol versions for peer and connect addr"
                            );
                            tcp_errors().invalid_proto_match.inc();
                            continue;
                        }
                    };

                    let setting_tcp_no_delay = self.settings.tcp_no_delay;
                    let certs = self.lookup.cert_manager().cloned();
                    let terminated_active = self.terminated_active.clone();

                    let event_tx = self.events_tx.clone();
                    let stats = self.stats.clone();
                    let cancel = self.cancel.child_token();
                    tokio::spawn(async move {
                        let terminated = match &found.target {
                            OriginTarget::HttpsTerminated { .. } => {
                                let Some(terminated) = prepare_terminated(
                                    &found,
                                    details.port_offset,
                                    details.peer_addr,
                                    &proxy_header,
                                    certs,
                                    &stats,
                                    &cancel,
                                )
                                .await
                                else {
                                    return;
                                };
                                Some(terminated)
                            }
                            _ => None,
                        };

                        let origin_addr = if terminated.is_some() {
                            // Unused: the proxy opens its own connections to the local server.
                            SocketAddr::from(([0, 0, 0, 0], 0))
                        } else {
                            let Some(origin_addr) = found.resolve_local(details.port_offset).await
                            else {
                                tracing::error!(
                                    port_offset = details.port_offset,
                                    tunnel_id = details.tunnel_id,
                                    "port offset not valid for tunnel"
                                );
                                tcp_errors().new_client_invalid_port_offset.inc();
                                return;
                            };
                            origin_addr
                        };

                        /* connect to tunnel server */

                        let conn_res = tokio::select! {
                            _ = cancel.cancelled() => return,
                            res = tokio::time::timeout(
                                Duration::from_secs(8),
                                TcpStream::connect(details.claim_instructions.address),
                            ) => res,
                        };

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

                        let send_res = tokio::select! {
                            _ = cancel.cancelled() => return,
                            res = tokio::time::timeout(
                                Duration::from_secs(8),
                                tunn_stream.write_all(&details.claim_instructions.token),
                            ) => res,
                        };
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
                        let confirm_res = tokio::select! {
                            _ = cancel.cancelled() => return,
                            res = tokio::time::timeout(
                                Duration::from_secs(4),
                                tunn_stream.read_exact(&mut expect_buffer[..]),
                            ) => res,
                        };
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

                        if let Some(terminated) = terminated {
                            // serve() returns only after upgraded connections and other
                            // tasks it spawned have ended, so they stay counted.
                            let _active = ActiveConnection::new(terminated_active, stats.clone());
                            cancel
                                .run_until_cancelled(terminated.serve(tunn_stream))
                                .await;
                            return;
                        }

                        /* connect to origin */

                        let connect_res = tokio::select! {
                            _ = cancel.cancelled() => return,
                            res = tokio::time::timeout(
                                Duration::from_secs(2),
                                LanAddress::tcp_socket(true, details.peer_addr, origin_addr),
                            ) => res,
                        };

                        let mut origin_stream = match connect_res {
                            Ok(Ok(stream)) => stream,
                            Ok(Err(error)) => {
                                tracing::error!(
                                    ?error,
                                    %origin_addr,
                                    tunnel_id = details.tunnel_id,
                                    port_offset = details.port_offset,
                                    source_addr = %details.peer_addr,
                                    "failed to connect to local TCP server; check that your server is running and listening on the configured local address"
                                );
                                tcp_errors().new_client_origin_connect_error.inc();
                                return;
                            }
                            Err(_) => {
                                tracing::error!(
                                    %origin_addr,
                                    tunnel_id = details.tunnel_id,
                                    port_offset = details.port_offset,
                                    source_addr = %details.peer_addr,
                                    "timed out connecting to local TCP server; check firewall rules and that the server is listening on the configured local address"
                                );
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
                                tokio::select! {
                                    _ = cancel.cancelled() => return,
                                    res = tokio::time::timeout(
                                        Duration::from_secs(2),
                                        proxy_header.write_v1_tcp(&mut origin_stream),
                                    ) => res,
                                }
                            }
                            Some(ProxyProtocol::ProxyProtocolV2) => {
                                tokio::select! {
                                    _ = cancel.cancelled() => return,
                                    res = tokio::time::timeout(
                                        Duration::from_secs(2),
                                        proxy_header.write_v2_tcp(&mut origin_stream),
                                    ) => res,
                                }
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

                        let tcp_client =
                            TcpClient::create_with_stats(tunn_stream, origin_stream, Some(stats))
                                .await;
                        let event = Event::ConnectedClient(Client {
                            id: client_id,
                            added_at: now_milli(),
                            tunnel_id: details.tunnel_id,
                            port_offset: details.port_offset,
                            source_addr: details.peer_addr,
                            tunnel_addr: details.connect_addr,
                            origin_addr,
                            tcp: tcp_client,
                        });
                        let _ = tokio::select! {
                            _ = cancel.cancelled() => return,
                            res = event_tx.send(event) => res,
                        };
                    });
                }
                Event::GetDetails(resp) => {
                    let _ = resp.send(self.clients.iter().map(Client::details).collect());
                }
                Event::ConnectedClient(client) => {
                    self.clients.push(client);
                    self.update_tcp_count();
                }
                Event::ClearOld => {
                    let now = now_milli();
                    self.clients.retain(|client| {
                        let last_use = client.tcp.last_use();

                        let since_tunn = now.max(last_use.tunn_to_origin) - last_use.tunn_to_origin;
                        let since_orig = now.max(last_use.origin_to_tunn) - last_use.origin_to_tunn;

                        if 90_000 < since_tunn && 30_000 < since_orig {
                            tracing::debug!(id = client.id, "clear old: 90s since tunnel data");
                            return false;
                        }

                        if 90_000 < since_orig && 30_000 < since_tunn {
                            tracing::debug!(id = client.id, "clear old: 90s since origin data");
                            return false;
                        }

                        if 60_000 < since_tunn && 60_000 < since_orig {
                            tracing::debug!(id = client.id, "clear old: 60s since any data");
                            return false;
                        }

                        true
                    });

                    // Update active TCP connection count
                    self.update_tcp_count();
                }
            }
        }
    }
}

/// How a connection to a TLS-terminated HTTPS tunnel is served.
enum Terminated {
    /// The HTTP port: Let's Encrypt challenges and redirects to HTTPS.
    Redirect {
        certs: CertManager,
        domain: String,
        stats: AgentStats,
    },
    /// The HTTPS port: TLS terminated here and requests forwarded to the local server.
    Https {
        acceptor: tokio_rustls::TlsAcceptor,
        ctx: ProxyContext,
    },
}

impl Terminated {
    async fn serve(self, tunnel: TcpStream) {
        match self {
            Terminated::Redirect {
                certs,
                domain,
                stats,
            } => serve_http_redirect(tunnel, certs, domain, Some(stats)).await,
            Terminated::Https { acceptor, ctx } => serve_https(tunnel, acceptor, ctx).await,
        }
    }
}

/// Decides how to serve a connection before claiming it, so connections that can't be
/// served yet (no certificate) are left unclaimed.
async fn prepare_terminated(
    found: &OriginResource,
    port_offset: u16,
    peer_addr: SocketAddr,
    proxy_header: &ProxyProtocolHeader,
    certs: Option<CertManager>,
    stats: &AgentStats,
    cancel: &CancellationToken,
) -> Option<Terminated> {
    let OriginTarget::HttpsTerminated { domain, .. } = &found.target else {
        return None;
    };
    let Some(certs) = certs else {
        tracing::error!(
            tunnel_id = found.tunnel_id,
            "tunnel is set to terminate TLS but this agent has no certificate directory"
        );
        return None;
    };
    let Some(domain) = domain.clone() else {
        tracing::error!(
            tunnel_id = found.tunnel_id,
            "tunnel is set to terminate TLS but its address is not a domain name"
        );
        return None;
    };

    match port_offset {
        0 => Some(Terminated::Redirect {
            certs,
            domain,
            stats: stats.clone(),
        }),
        1 => {
            let Some(acceptor) = certs.acceptor(&domain) else {
                tracing::warn!(%domain, "HTTPS connection dropped because the TLS certificate is not ready yet");
                return None;
            };
            let Some(upstream) = found.target.resolve_terminated_upstream().await else {
                tracing::error!(
                    %domain,
                    "no local server for TLS-terminated tunnel; set the HTTP or HTTPS port to a non-zero value"
                );
                return None;
            };
            Some(Terminated::Https {
                acceptor,
                ctx: ProxyContext {
                    domain,
                    upstream,
                    peer_addr,
                    proxy_protocol: found.proxy_protocol.map(|p| (p, proxy_header.clone())),
                    stats: Some(stats.clone()),
                    cancel: cancel.clone(),
                },
            })
        }
        _ => {
            tcp_errors().new_client_invalid_port_offset.inc();
            None
        }
    }
}

/// Counts a proxied connection as active for as long as it is served.
struct ActiveConnection {
    count: Arc<AtomicU32>,
    stats: AgentStats,
}

impl ActiveConnection {
    fn new(count: Arc<AtomicU32>, stats: AgentStats) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        stats.inc_tcp();
        ActiveConnection { count, stats }
    }
}

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::Relaxed);
        self.stats.dec_tcp();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::origin_lookup::OriginIp;
    use crate::tls::{CertStore, proxy::Upstream};
    use playit_agent_proto::PortProto;

    const DOMAIN: &str = "site.example.com";

    fn terminated(domain: Option<&str>, http_port: u16, https_port: u16) -> OriginResource {
        OriginResource {
            tunnel_id: 7,
            proto: PortProto::Tcp,
            target: OriginTarget::HttpsTerminated {
                ip: OriginIp::IpAddress("127.0.0.1".parse().unwrap()),
                domain: domain.map(str::to_string),
                http_port,
                https_port,
            },
            port_count: 2,
            proxy_protocol: Some(ProxyProtocol::ProxyProtocolV2),
        }
    }

    fn header() -> ProxyProtocolHeader {
        ProxyProtocolHeader::AfInet {
            client_ip: "203.0.113.7".parse().unwrap(),
            proxy_ip: "198.51.100.1".parse().unwrap(),
            client_port: 51000,
            proxy_port: 443,
        }
    }

    fn temp_store() -> CertStore {
        CertStore::new(std::env::temp_dir().join(format!(
            "playit-tcp-clients-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }

    /// A manager that holds a certificate for `DOMAIN`, loaded from its store.
    async fn manager_with_certificate() -> CertManager {
        let store = temp_store();
        let cert = rcgen::generate_simple_self_signed(vec![DOMAIN.to_string()]).unwrap();
        store
            .save_cert(
                DOMAIN,
                &crate::tls::store::StoredCert {
                    chain_pem: cert.cert.pem(),
                    key_pem: cert.signing_key.serialize_pem(),
                },
            )
            .await
            .unwrap();

        let manager = CertManager::new(store);
        manager.set_domains([DOMAIN.to_string()]);
        let cancel = CancellationToken::new();
        tokio::spawn(manager.clone().run(cancel.clone()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while manager.acceptor(DOMAIN).is_none() {
            assert!(Instant::now() < deadline, "certificate was not loaded");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cancel.cancel();
        let _ = std::fs::remove_dir_all(manager.store().dir());
        manager
    }

    async fn dispatch(
        found: &OriginResource,
        port_offset: u16,
        certs: Option<CertManager>,
    ) -> Option<Terminated> {
        prepare_terminated(
            found,
            port_offset,
            "203.0.113.7:51000".parse().unwrap(),
            &header(),
            certs,
            &AgentStats::new(),
            &CancellationToken::new(),
        )
        .await
    }

    #[tokio::test]
    async fn http_port_is_served_by_the_redirect_handler() {
        let certs = CertManager::new(temp_store());
        let served = dispatch(&terminated(Some(DOMAIN), 8080, 0), 0, Some(certs)).await;
        match served {
            Some(Terminated::Redirect { domain, .. }) => assert_eq!(domain, DOMAIN),
            _ => panic!("expected the redirect handler"),
        }
    }

    #[tokio::test]
    async fn https_port_is_served_only_with_a_certificate() {
        let found = terminated(Some(DOMAIN), 8080, 0);
        let without = dispatch(&found, 1, Some(CertManager::new(temp_store()))).await;
        assert!(without.is_none());

        let served = dispatch(&found, 1, Some(manager_with_certificate().await)).await;
        match served {
            Some(Terminated::Https { ctx, .. }) => {
                assert_eq!(ctx.domain, DOMAIN);
                assert_eq!(
                    ctx.upstream,
                    Upstream::Http("127.0.0.1:8080".parse().unwrap())
                );
                assert_eq!(ctx.peer_addr, "203.0.113.7:51000".parse().unwrap());
                assert_eq!(
                    ctx.proxy_protocol,
                    Some((ProxyProtocol::ProxyProtocolV2, header()))
                );
            }
            _ => panic!("expected the HTTPS proxy"),
        }
    }

    #[tokio::test]
    async fn https_port_without_a_local_server_is_rejected() {
        let found = terminated(Some(DOMAIN), 0, 0);
        assert!(
            dispatch(&found, 1, Some(manager_with_certificate().await))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn other_port_offsets_are_rejected() {
        let found = terminated(Some(DOMAIN), 8080, 0);
        let certs = manager_with_certificate().await;
        assert!(dispatch(&found, 2, Some(certs.clone())).await.is_none());
        assert!(dispatch(&found, u16::MAX, Some(certs)).await.is_none());
    }

    #[tokio::test]
    async fn missing_domain_or_cert_manager_is_rejected() {
        let certs = manager_with_certificate().await;
        let no_domain = terminated(None, 8080, 0);
        assert!(dispatch(&no_domain, 0, Some(certs.clone())).await.is_none());
        assert!(dispatch(&no_domain, 1, Some(certs)).await.is_none());

        let found = terminated(Some(DOMAIN), 8080, 0);
        assert!(dispatch(&found, 0, None).await.is_none());
        assert!(dispatch(&found, 1, None).await.is_none());
    }

    #[tokio::test]
    async fn plain_tunnels_are_not_terminated() {
        let found = OriginResource {
            target: OriginTarget::Port {
                ip: OriginIp::IpAddress("127.0.0.1".parse().unwrap()),
                port: 8080,
            },
            ..terminated(Some(DOMAIN), 8080, 0)
        };
        let certs = manager_with_certificate().await;
        assert!(dispatch(&found, 0, Some(certs)).await.is_none());
    }
}
