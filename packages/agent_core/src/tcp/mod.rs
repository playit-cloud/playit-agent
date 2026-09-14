//! TCP tunnels.
//!
//! Every `NewClient` from the control channel becomes one task: claim the connection at
//! the tunnel server, connect to the local origin, then pipe bytes both ways until either
//! side closes, an error occurs, the connection sits idle, or the agent shuts down.

mod connect;
mod pipe;

use std::{
    net::SocketAddr,
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_agent_proto::control_feed::NewClient;
use serde::Serialize;
use slotmap::{Key, SlotMap};
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

use crate::{
    origin::{OriginLookup, Transport, proxy_protocol::ProxyProtocolHeader},
    stats::AgentStats,
    util::now_milli,
};

#[derive(Debug, Clone)]
pub struct TcpSettings {
    /// New clients accepted per second (steady state) and the burst allowed on top.
    pub new_client_ratelimit: u32,
    pub new_client_ratelimit_burst: u32,
    /// `TCP_NODELAY` for the tunnel-side stream. The origin-side stream always has it.
    pub tcp_no_delay: bool,
    /// Bound on connecting to the tunnel server and completing the claim handshake.
    pub claim_timeout: Duration,
    /// Bound on connecting to the origin and writing the proxy protocol header.
    pub origin_connect_timeout: Duration,
    /// A connection with no bytes in either direction for this long is closed.
    pub idle_timeout: Duration,
    /// Bind to a per-client `127.x.y.z` address when the origin is on IPv4 loopback.
    pub client_loopback_ip: bool,
}

impl Default for TcpSettings {
    fn default() -> Self {
        TcpSettings {
            new_client_ratelimit: 100,
            new_client_ratelimit_burst: 300,
            tcp_no_delay: true,
            claim_timeout: Duration::from_secs(10),
            origin_connect_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(60),
            client_loopback_ip: true,
        }
    }
}

slotmap::new_key_type! {
    struct ConnectionKey;
}

/// Accepts new clients and tracks the resulting connections.
///
/// Dropping it cancels every connection it started.
pub struct TcpTunnels {
    shared: Arc<Shared>,
}

struct Shared {
    settings: TcpSettings,
    lookup: Arc<OriginLookup>,
    stats: AgentStats,
    limiter: DefaultDirectRateLimiter,
    cancel: CancellationToken,
    connections: Mutex<SlotMap<ConnectionKey, Arc<ConnectionInfo>>>,
}

impl Drop for TcpTunnels {
    fn drop(&mut self) {
        self.shared.cancel.cancel();
    }
}

/// Live counters for one tunneled connection.
pub struct ConnectionInfo {
    pub tunnel_id: u64,
    pub port_offset: u16,
    pub peer_addr: SocketAddr,
    pub tunnel_addr: SocketAddr,
    pub origin_addr: SocketAddr,
    pub opened_at_ms: u64,
    opened_at: Instant,
    bytes_to_origin: AtomicU64,
    bytes_to_tunnel: AtomicU64,
    /// Milliseconds since `opened_at` of the last byte moved in either direction.
    last_activity_ms: AtomicU64,
}

impl ConnectionInfo {
    fn new(client: &NewClient, origin_addr: SocketAddr) -> Self {
        ConnectionInfo {
            tunnel_id: client.tunnel_id,
            port_offset: client.port_offset,
            peer_addr: client.peer_addr,
            tunnel_addr: client.connect_addr,
            origin_addr,
            opened_at_ms: now_milli(),
            opened_at: Instant::now(),
            bytes_to_origin: AtomicU64::new(0),
            bytes_to_tunnel: AtomicU64::new(0),
            last_activity_ms: AtomicU64::new(0),
        }
    }

    fn touch(&self) {
        let elapsed = self.opened_at.elapsed().as_millis() as u64;
        self.last_activity_ms.store(elapsed, Ordering::Relaxed);
    }

    fn record_to_origin(&self, bytes: usize) {
        self.bytes_to_origin
            .fetch_add(bytes as u64, Ordering::Relaxed);
        self.touch();
    }

    fn record_to_tunnel(&self, bytes: usize) {
        self.bytes_to_tunnel
            .fetch_add(bytes as u64, Ordering::Relaxed);
        self.touch();
    }

    pub fn last_activity(&self) -> Instant {
        self.opened_at + Duration::from_millis(self.last_activity_ms.load(Ordering::Relaxed))
    }

    pub fn bytes_to_origin(&self) -> u64 {
        self.bytes_to_origin.load(Ordering::Relaxed)
    }

    pub fn bytes_to_tunnel(&self) -> u64 {
        self.bytes_to_tunnel.load(Ordering::Relaxed)
    }

    fn details(&self, id: u64) -> TcpConnectionDetails {
        TcpConnectionDetails {
            id,
            tunnel_id: self.tunnel_id,
            port_offset: self.port_offset,
            peer_addr: self.peer_addr,
            tunnel_addr: self.tunnel_addr,
            origin_addr: self.origin_addr,
            opened_at: self.opened_at_ms,
            idle_ms: self.last_activity().elapsed().as_millis() as u64,
            bytes_to_origin: self.bytes_to_origin(),
            bytes_to_tunnel: self.bytes_to_tunnel(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TcpConnectionDetails {
    pub id: u64,
    pub tunnel_id: u64,
    pub port_offset: u16,
    pub peer_addr: SocketAddr,
    pub tunnel_addr: SocketAddr,
    pub origin_addr: SocketAddr,
    /// Unix milliseconds.
    pub opened_at: u64,
    pub idle_ms: u64,
    pub bytes_to_origin: u64,
    pub bytes_to_tunnel: u64,
}

impl TcpTunnels {
    pub fn new(
        settings: TcpSettings,
        lookup: Arc<OriginLookup>,
        stats: AgentStats,
        cancel: CancellationToken,
    ) -> Self {
        let limiter = RateLimiter::direct(new_client_quota(&settings));
        TcpTunnels {
            shared: Arc::new(Shared {
                settings,
                lookup,
                stats,
                limiter,
                cancel,
                connections: Mutex::new(SlotMap::with_key()),
            }),
        }
    }

    /// Starts claiming and piping the client. Returns immediately.
    pub fn handle_new_client(&self, client: NewClient) {
        if self.shared.limiter.check().is_err() {
            self.shared.stats.tcp().rate_limited.inc();
            tracing::warn!(tunnel_id = client.tunnel_id, peer = %client.peer_addr, "new tcp client rate limited");
            return;
        }

        let shared = self.shared.clone();
        tokio::spawn(async move {
            let cancel = shared.cancel.clone();
            cancel.run_until_cancelled(run_client(shared, client)).await;
        });
    }

    pub fn active(&self) -> usize {
        self.lock_connections().len()
    }

    pub fn connections(&self) -> Vec<TcpConnectionDetails> {
        self.lock_connections()
            .iter()
            .map(|(key, info)| info.details(key.data().as_ffi()))
            .collect()
    }

    fn lock_connections(
        &self,
    ) -> std::sync::MutexGuard<'_, SlotMap<ConnectionKey, Arc<ConnectionInfo>>> {
        self.shared
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}

fn new_client_quota(settings: &TcpSettings) -> Quota {
    let rate = NonZeroU32::new(settings.new_client_ratelimit).unwrap_or_else(|| {
        tracing::warn!("tcp new client rate limit of 0 clamped to 1");
        NonZeroU32::MIN
    });
    let burst = NonZeroU32::new(settings.new_client_ratelimit_burst).unwrap_or_else(|| {
        tracing::warn!("tcp new client burst of 0 clamped to 1");
        NonZeroU32::MIN
    });
    Quota::per_second(rate).allow_burst(burst)
}

/// Removes the connection from the table when the connection task ends.
struct Tracked {
    shared: Arc<Shared>,
    key: ConnectionKey,
}

impl Shared {
    fn track(self: &Arc<Self>, info: Arc<ConnectionInfo>) -> Tracked {
        let mut connections = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        let key = connections.insert(info);
        self.stats.set_active_tcp(connections.len() as u32);
        Tracked {
            shared: self.clone(),
            key,
        }
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        let mut connections = self
            .shared
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        connections.remove(self.key);
        self.shared.stats.set_active_tcp(connections.len() as u32);
    }
}

async fn run_client(shared: Arc<Shared>, client: NewClient) {
    let counters = shared.stats.tcp();
    tracing::info!(
        tunnel_id = client.tunnel_id,
        port_offset = client.port_offset,
        peer = %client.peer_addr,
        tunnel = %client.connect_addr,
        "new tcp client"
    );

    let Some(origin) = shared.lookup.lookup(client.tunnel_id, Transport::Tcp) else {
        tracing::warn!(
            tunnel_id = client.tunnel_id,
            "no origin configured for tunnel"
        );
        counters.origin_not_found.inc();
        return;
    };

    let Some(origin_addr) = origin.resolve_local(client.port_offset).await else {
        tracing::warn!(
            tunnel_id = client.tunnel_id,
            port_offset = client.port_offset,
            "port offset not valid for tunnel"
        );
        counters.invalid_port_offset.inc();
        return;
    };

    let proxy_header = match origin.proxy_protocol {
        None => None,
        Some(version) => {
            match ProxyProtocolHeader::from_addrs(client.peer_addr, client.connect_addr) {
                Some(header) => Some((version, header)),
                None => {
                    tracing::error!(
                        peer = %client.peer_addr,
                        tunnel = %client.connect_addr,
                        "tunnel server sent peer and tunnel addresses of different families"
                    );
                    counters.address_family_mismatch.inc();
                    return;
                }
            }
        }
    };

    let tunnel_stream = match timeout(
        shared.settings.claim_timeout,
        connect::claim(&client.claim_instructions, shared.settings.tcp_no_delay),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            tracing::error!(?error, claim = %client.claim_instructions.address, "failed to claim tcp client");
            counters.claim_failed.inc();
            return;
        }
        Err(_) => {
            tracing::error!(claim = %client.claim_instructions.address, "timeout claiming tcp client");
            counters.claim_failed.inc();
            return;
        }
    };

    let origin_stream = match timeout(
        shared.settings.origin_connect_timeout,
        connect::origin(
            shared.settings.client_loopback_ip,
            client.peer_addr,
            origin_addr,
            proxy_header,
        ),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(connect::OriginError::Connect(error))) => {
            tracing::error!(
                ?error,
                %origin_addr,
                tunnel_id = client.tunnel_id,
                port_offset = client.port_offset,
                peer = %client.peer_addr,
                "failed to connect to local TCP server; check that your server is running and listening on the configured local address"
            );
            counters.origin_connect_failed.inc();
            return;
        }
        Ok(Err(connect::OriginError::ProxyHeader(error))) => {
            tracing::error!(?error, %origin_addr, "failed to write proxy protocol header");
            counters.proxy_header_failed.inc();
            return;
        }
        Err(_) => {
            tracing::error!(
                %origin_addr,
                tunnel_id = client.tunnel_id,
                port_offset = client.port_offset,
                peer = %client.peer_addr,
                "timed out connecting to local TCP server; check firewall rules and that the server is listening on the configured local address"
            );
            counters.origin_connect_failed.inc();
            return;
        }
    };

    let info = Arc::new(ConnectionInfo::new(&client, origin_addr));
    let _tracked = shared.track(info.clone());

    let outcome = pipe::run(
        tunnel_stream,
        origin_stream,
        &info,
        &shared.stats,
        shared.settings.idle_timeout,
    )
    .await;

    tracing::debug!(
        ?outcome,
        peer = %client.peer_addr,
        %origin_addr,
        bytes_to_origin = info.bytes_to_origin(),
        bytes_to_tunnel = info.bytes_to_tunnel(),
        "tcp client closed"
    );
}

pub use pipe::PipeOutcome;
