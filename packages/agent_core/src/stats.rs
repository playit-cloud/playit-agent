use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::network::tcp::tcp_errors::TcpErrors;
use crate::network::udp::udp_errors::UdpErrors;

/// Shared statistics for the agent
#[derive(Debug, Default, Clone)]
pub struct AgentStats {
    inner: Arc<StatsInner>,
}

#[derive(Debug, Default)]
struct StatsInner {
    /// Bytes received from tunnel (incoming to local)
    pub bytes_in: AtomicU64,
    /// Bytes sent to tunnel (outgoing from local)
    pub bytes_out: AtomicU64,
    /// Active TCP connections
    pub active_tcp: AtomicU32,
    /// Active UDP flows
    pub active_udp: AtomicU32,
    pub tcp_errors: TcpErrors,
    pub udp_errors: UdpErrors,
}

impl AgentStats {
    pub fn new() -> Self {
        AgentStats {
            inner: Arc::new(StatsInner::default()),
        }
    }

    /// Add bytes received from tunnel
    pub fn add_bytes_in(&self, bytes: u64) {
        self.inner.bytes_in.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Add bytes sent to tunnel
    pub fn add_bytes_out(&self, bytes: u64) {
        self.inner.bytes_out.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Increment active TCP connections
    pub fn inc_tcp(&self) {
        self.inner.active_tcp.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement active TCP connections
    pub fn dec_tcp(&self) {
        self.inner.active_tcp.fetch_sub(1, Ordering::Relaxed);
    }

    /// Set active TCP connection count
    pub fn set_tcp(&self, count: u32) {
        self.inner.active_tcp.store(count, Ordering::Relaxed);
    }

    /// Increment active UDP flows
    pub fn inc_udp(&self) {
        self.inner.active_udp.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement active UDP flows
    pub fn dec_udp(&self) {
        self.inner.active_udp.fetch_sub(1, Ordering::Relaxed);
    }

    /// Set active UDP flow count
    pub fn set_udp(&self, count: u32) {
        self.inner.active_udp.store(count, Ordering::Relaxed);
    }

    /// Get current bytes received from tunnel
    pub fn bytes_in(&self) -> u64 {
        self.inner.bytes_in.load(Ordering::Relaxed)
    }

    /// Get current bytes sent to tunnel
    pub fn bytes_out(&self) -> u64 {
        self.inner.bytes_out.load(Ordering::Relaxed)
    }

    /// Get active TCP connection count
    pub fn active_tcp(&self) -> u32 {
        self.inner.active_tcp.load(Ordering::Relaxed)
    }

    /// Get active UDP flow count
    pub fn active_udp(&self) -> u32 {
        self.inner.active_udp.load(Ordering::Relaxed)
    }

    pub fn tcp_errors(&self) -> &TcpErrors {
        &self.inner.tcp_errors
    }

    pub fn udp_errors(&self) -> &UdpErrors {
        &self.inner.udp_errors
    }

    /// Get a snapshot of all stats
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            bytes_in: self.bytes_in(),
            bytes_out: self.bytes_out(),
            active_tcp: self.active_tcp(),
            active_udp: self.active_udp(),
            tcp_errors: serde_json::to_value(self.tcp_errors())
                .expect("network error counters must serialize"),
            udp_errors: serde_json::to_value(self.udp_errors())
                .expect("network error counters must serialize"),
        }
    }
}

/// A snapshot of stats at a point in time
#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_tcp: u32,
    pub active_udp: u32,
    pub tcp_errors: serde_json::Value,
    pub udp_errors: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::AgentStats;

    #[test]
    fn network_error_counters_are_instance_scoped_and_snapshotted() {
        let first = AgentStats::new();
        let second = AgentStats::new();

        first.udp_errors().recv_io_error.inc();
        first.tcp_errors().new_client_rate_limited.inc();

        assert_eq!(first.snapshot().udp_errors["recv_io_error"], 1);
        assert_eq!(first.snapshot().tcp_errors["new_client_rate_limited"], 1);
        assert_eq!(second.snapshot().udp_errors["recv_io_error"], 0);
        assert_eq!(second.snapshot().tcp_errors["new_client_rate_limited"], 0);
    }
}
