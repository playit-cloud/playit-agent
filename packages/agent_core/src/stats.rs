use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

use serde::Serialize;

/// Shared, lock-free counters for one agent. Cheap to clone; all clones share state.
#[derive(Debug, Default, Clone)]
pub struct AgentStats {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    active_tcp: AtomicU32,
    active_udp: AtomicU32,
    tcp: TcpCounters,
    udp: UdpCounters,
}

/// Monotonic event counter.
#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

impl Serialize for Counter {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.get())
    }
}

/// Why TCP clients were dropped before a connection was established.
#[derive(Debug, Default, Serialize)]
pub struct TcpCounters {
    pub rate_limited: Counter,
    pub origin_not_found: Counter,
    pub invalid_port_offset: Counter,
    pub address_family_mismatch: Counter,
    pub claim_failed: Counter,
    pub origin_connect_failed: Counter,
    pub proxy_header_failed: Counter,
}

/// Why UDP datagrams were dropped.
#[derive(Debug, Default, Serialize)]
pub struct UdpCounters {
    pub rate_limited: Counter,
    pub origin_not_found: Counter,
    pub invalid_port_offset: Counter,
    pub no_session: Counter,
    pub unexpected_source: Counter,
    pub invalid_packet: Counter,
    pub bind_failed: Counter,
    pub origin_send_failed: Counter,
    pub tunnel_send_failed: Counter,
    pub stale_flow: Counter,
    pub unsupported_proxy_protocol: Counter,
    pub recv_failed: Counter,
}

impl AgentStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes delivered from the tunnel to local origins.
    pub fn add_bytes_in(&self, bytes: u64) {
        self.inner.bytes_in.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Bytes delivered from local origins to the tunnel.
    pub fn add_bytes_out(&self, bytes: u64) {
        self.inner.bytes_out.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn set_active_tcp(&self, count: u32) {
        self.inner.active_tcp.store(count, Ordering::Relaxed);
    }

    pub fn set_active_udp(&self, count: u32) {
        self.inner.active_udp.store(count, Ordering::Relaxed);
    }

    pub fn tcp(&self) -> &TcpCounters {
        &self.inner.tcp
    }

    pub fn udp(&self) -> &UdpCounters {
        &self.inner.udp
    }

    pub fn bytes_in(&self) -> u64 {
        self.inner.bytes_in.load(Ordering::Relaxed)
    }

    pub fn bytes_out(&self) -> u64 {
        self.inner.bytes_out.load(Ordering::Relaxed)
    }

    pub fn active_tcp(&self) -> u32 {
        self.inner.active_tcp.load(Ordering::Relaxed)
    }

    pub fn active_udp(&self) -> u32 {
        self.inner.active_udp.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            bytes_in: self.bytes_in(),
            bytes_out: self.bytes_out(),
            active_tcp: self.active_tcp(),
            active_udp: self.active_udp(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StatsSnapshot {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_tcp: u32,
    pub active_udp: u32,
}
