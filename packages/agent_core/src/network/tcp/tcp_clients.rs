use std::{
    collections::HashSet,
    net::SocketAddr,
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_agent_proto::control_feed::{ClaimInstructions, NewClient};
use serde::Serialize;
use tokio::{
    sync::mpsc::{Receiver, Sender, channel},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{network::origin_lookup::OriginLookup, stats::AgentStats, utils::now_milli};

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
    task: tokio::task::JoinHandle<()>,
}

struct Worker {
    lookup: Arc<OriginLookup>,
    events: Receiver<Event>,
    cancel: CancellationToken,
    settings: TcpSettings,
    stats: AgentStats,

    clients: Vec<Client>,
    claims: Arc<Mutex<HashSet<ClaimInstructions>>>,
    next_client_id: u64,
    pending: tokio::task::JoinSet<Option<Client>>,
}

struct ClaimReservation {
    claim: ClaimInstructions,
    claims: Arc<Mutex<HashSet<ClaimInstructions>>>,
}

impl Drop for ClaimReservation {
    fn drop(&mut self) {
        self.claims.lock().unwrap().remove(&self.claim);
    }
}

struct Client {
    _claim: ClaimReservation,
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

    GetDetails(tokio::sync::oneshot::Sender<Vec<TcpClientDetails>>),
}

impl TcpClients {
    pub fn new(
        settings: TcpSettings,
        lookup: Arc<OriginLookup>,
        stats: AgentStats,
        cancel: CancellationToken,
    ) -> Self {
        let cancel = cancel.child_token();
        let quota = build_quota(&settings);
        let (events_tx, events_rx) = channel(1024);

        let task = tokio::spawn(
            Worker {
                next_client_id: 1,
                lookup,
                events: events_rx,
                pending: tokio::task::JoinSet::new(),
                cancel: cancel.child_token(),
                settings,
                stats,
                clients: Vec::with_capacity(32),
                claims: Arc::new(Mutex::new(HashSet::new())),
            }
            .start(),
        );

        TcpClients {
            new_client_limiter: RateLimiter::direct(quota),
            events_tx,
            cancel,
            task,
        }
    }

    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        let _ = (&mut self.task).await;
    }

    pub async fn get_details(&self) -> Vec<TcpClientDetails> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self.events_tx.send(Event::GetDetails(tx)).await;
        rx.await.unwrap_or_default()
    }

    pub async fn handle_new_client(&self, new_client: NewClient) {
        if self.new_client_limiter.check().is_err() {
            tcp_errors().new_client_rate_limited.inc();
            return;
        }

        let _ = self.events_tx.send(Event::NewClient(new_client)).await;
    }
}

impl Drop for TcpClients {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stats.set_tcp(0);
    }
}

impl Worker {
    pub async fn start(mut self) {
        let mut next_clear = Instant::now() + Duration::from_secs(15);

        loop {
            let event = tokio::select! {
                result = self.pending.join_next(), if !self.pending.is_empty() => {
                    match result {
                        Some(Ok(Some(client))) => self.clients.push(client),
                        Some(Err(error)) => tracing::error!(?error, "TCP setup task failed"),
                        _ => {}
                    }
                    self.stats.set_tcp(self.clients.len() as u32);
                    continue;
                }
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
                    if self.pending.len() >= self.settings.max_pending_clients
                        || self.pending.len() + self.clients.len() >= self.settings.max_clients
                    {
                        tcp_errors().new_client_rate_limited.inc();
                        continue;
                    }
                    if !self
                        .claims
                        .lock()
                        .unwrap()
                        .insert(details.claim_instructions.clone())
                    {
                        continue;
                    }
                    let claim = ClaimReservation {
                        claim: details.claim_instructions.clone(),
                        claims: self.claims.clone(),
                    };
                    let client_id = self.next_client_id;
                    self.next_client_id = client_id + 1;

                    tracing::info!(?details, id = client_id, "New TCP Client");

                    let setting_tcp_no_delay = self.settings.tcp_no_delay;

                    let lookup = self.lookup.clone();
                    let stats = self.stats.clone();
                    self.pending.spawn(async move {
                        let connection = tokio::time::timeout(
                            Duration::from_secs(30),
                            super::tcp_setup::connect(&details, &lookup, setting_tcp_no_delay),
                        )
                        .await;
                        let connection = match connection {
                            Ok(Ok(connection)) => connection,
                            Ok(Err(error)) => {
                                tracing::debug!(id = client_id, ?error, "TCP setup failed");
                                return None;
                            }
                            Err(_) => {
                                tracing::debug!(id = client_id, "TCP setup deadline exceeded");
                                return None;
                            }
                        };
                        let tcp = TcpClient::create_with_stats(
                            connection.tunnel,
                            connection.origin,
                            Some(stats),
                        )
                        .await;
                        Some(Client {
                            _claim: claim,
                            id: client_id,
                            added_at: now_milli(),
                            tunnel_id: details.tunnel_id,
                            port_offset: details.port_offset,
                            source_addr: details.peer_addr,
                            tunnel_addr: details.connect_addr,
                            origin_addr: connection.origin_addr,
                            tcp,
                        })
                    });
                }
                Event::GetDetails(resp) => {
                    let _ = resp.send(self.clients.iter().map(Client::details).collect());
                }
                Event::ClearOld => {
                    self.clients.retain(|client| {
                        !client.tcp.is_closed()
                            && client.tcp.idle_for() < self.settings.idle_timeout
                    });

                    // Update active TCP connection count
                    self.stats.set_tcp(self.clients.len() as u32);
                }
            }
        }
        self.pending.shutdown().await;
        self.clients.clear();
        self.stats.set_tcp(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::origin_lookup::{OriginIp, OriginResource, OriginTarget};
    use playit_agent_proto::PortProto;
    use playit_api_client::api::ProxyProtocol;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn fixture(
        max_pending: usize,
    ) -> (
        TcpClients,
        NewClient,
        TcpListener,
        TcpListener,
        CancellationToken,
    ) {
        let claims = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let lookup = Arc::new(OriginLookup::default());
        lookup
            .update(std::iter::once(OriginResource {
                tunnel_id: 42,
                proto: PortProto::Tcp,
                port_count: 1,
                target: OriginTarget::Port {
                    ip: OriginIp::IpAddress("127.0.0.1".parse().unwrap()),
                    port: origin.local_addr().unwrap().port(),
                },
                proxy_protocol: Some(ProxyProtocol::ProxyProtocolV1),
            }))
            .await;
        let parent = CancellationToken::new();
        let clients = TcpClients::new(
            TcpSettings {
                max_pending_clients: max_pending,
                ..Default::default()
            },
            lookup,
            AgentStats::new(),
            parent.clone(),
        );
        let details = NewClient {
            connect_addr: "203.0.113.1:4000".parse().unwrap(),
            peer_addr: "198.51.100.1:5000".parse().unwrap(),
            tunnel_id: 42,
            port_offset: 0,
            data_center_id: 1,
            claim_instructions: ClaimInstructions {
                address: claims.local_addr().unwrap(),
                token: b"token".to_vec(),
            },
        };
        (clients, details, claims, origin, parent)
    }

    #[tokio::test]
    async fn setup_relays_proxy_header_and_preserves_half_close() {
        let (clients, details, claims, origin, _) = fixture(8).await;
        clients.handle_new_client(details.clone()).await;
        let (mut tunnel, _) = tokio::time::timeout(Duration::from_secs(1), claims.accept())
            .await
            .unwrap()
            .unwrap();
        let mut token = [0; 5];
        tunnel.read_exact(&mut token).await.unwrap();
        assert_eq!(&token, b"token");
        clients.handle_new_client(details).await;
        tunnel.write_all(&[0; 8]).await.unwrap();
        let (mut server, _) = tokio::time::timeout(Duration::from_secs(1), origin.accept())
            .await
            .unwrap()
            .unwrap();
        tunnel.write_all(b"request").await.unwrap();
        tunnel.shutdown().await.unwrap();
        let mut request = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), server.read_to_end(&mut request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            request,
            b"PROXY TCP4 198.51.100.1 203.0.113.1 5000 4000\r\nrequest"
        );
        server.write_all(b"reply").await.unwrap();
        server.shutdown().await.unwrap();
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), tunnel.read_to_end(&mut reply))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply, b"reply");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), claims.accept())
                .await
                .is_err()
        );
        clients.shutdown().await;
    }

    #[tokio::test]
    async fn pending_limit_and_shutdown_cover_claim_handshake() {
        let (clients, mut details, claims, _origin, parent) = fixture(1).await;
        clients.handle_new_client(details.clone()).await;
        let (mut tunnel, _) = tokio::time::timeout(Duration::from_secs(1), claims.accept())
            .await
            .unwrap()
            .unwrap();
        let mut token = [0; 5];
        tunnel.read_exact(&mut token).await.unwrap();
        details.claim_instructions.token.push(1);
        clients.handle_new_client(details).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(20), claims.accept())
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(1), clients.shutdown())
            .await
            .unwrap();
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), tunnel.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert!(!parent.is_cancelled());
    }
}
