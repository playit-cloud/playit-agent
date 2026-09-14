//! Table of virtual UDP clients.
//!
//! Each remote client talking to a tunnel gets its own local socket toward the origin.
//! Replies on that socket are read by a small task and handed back to the tunnel loop
//! tagged with the flow key. Dropping a [`Flow`] cancels its task.

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use playit_agent_proto::udp_proto::UdpFlow;
use slotmap::SlotMap;
use tokio::{net::UdpSocket, sync::mpsc, time::Instant};
use tokio_util::sync::{CancellationToken, DropGuard};

use super::packet::{Packet, PacketPool};
use crate::stats::AgentStats;

slotmap::new_key_type! {
    pub struct FlowKey;
}

/// Identity of a virtual client: who is talking, to which tunnel, on which port of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientKey {
    pub client_addr: SocketAddr,
    pub tunnel_id: u64,
    pub port_offset: u16,
}

/// A datagram the origin sent back to a virtual client.
pub struct OriginPacket {
    pub flow: FlowKey,
    pub from: SocketAddr,
    pub packet: Packet,
}

pub struct Flow {
    pub client: ClientKey,
    pub socket: Arc<UdpSocket>,
    pub target: SocketAddr,
    /// Footer to attach to replies: the latest inbound flow, reversed.
    pub reply_flow: UdpFlow,
    pub opened_at: Instant,
    pub last_from_tunnel: Instant,
    pub last_from_origin: Instant,
    _receiver: DropGuard,
}

impl Flow {
    fn is_idle(&self, now: Instant, idle_timeout: Duration, one_way_timeout: Duration) -> bool {
        let since_tunnel = now.saturating_duration_since(self.last_from_tunnel);
        let since_origin = now.saturating_duration_since(self.last_from_origin);

        (idle_timeout <= since_tunnel && idle_timeout <= since_origin)
            || one_way_timeout <= since_tunnel
            || one_way_timeout <= since_origin
    }
}

pub struct FlowTable {
    flows: SlotMap<FlowKey, Flow>,
    by_client: HashMap<ClientKey, FlowKey>,
    pool: PacketPool,
    origin_tx: mpsc::Sender<OriginPacket>,
    stats: AgentStats,
}

impl FlowTable {
    pub fn new(pool: PacketPool, origin_tx: mpsc::Sender<OriginPacket>, stats: AgentStats) -> Self {
        FlowTable {
            flows: SlotMap::with_key(),
            by_client: HashMap::new(),
            pool,
            origin_tx,
            stats,
        }
    }

    pub fn len(&self) -> usize {
        self.flows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.flows.is_empty()
    }

    pub fn get(&self, key: FlowKey) -> Option<&Flow> {
        self.flows.get(key)
    }

    pub fn get_mut(&mut self, key: FlowKey) -> Option<&mut Flow> {
        self.flows.get_mut(key)
    }

    pub fn find_mut(&mut self, client: &ClientKey) -> Option<&mut Flow> {
        let key = *self.by_client.get(client)?;
        self.flows.get_mut(key)
    }

    /// Adds a flow and starts reading replies from its socket.
    pub fn insert(
        &mut self,
        client: ClientKey,
        socket: Arc<UdpSocket>,
        target: SocketAddr,
        reply_flow: UdpFlow,
        now: Instant,
    ) -> FlowKey {
        if let Some(previous) = self.by_client.remove(&client) {
            self.flows.remove(previous);
        }

        let cancel = CancellationToken::new();
        let key = self.flows.insert(Flow {
            client,
            socket: socket.clone(),
            target,
            reply_flow,
            opened_at: now,
            last_from_tunnel: now,
            last_from_origin: now,
            _receiver: cancel.clone().drop_guard(),
        });
        self.by_client.insert(client, key);

        tokio::spawn(cancel.run_until_cancelled_owned(receive_from_origin(
            key,
            socket,
            self.pool.clone(),
            self.origin_tx.clone(),
            self.stats.clone(),
        )));

        key
    }

    pub fn remove(&mut self, key: FlowKey) -> Option<Flow> {
        let flow = self.flows.remove(key)?;
        self.by_client.remove(&flow.client);
        Some(flow)
    }

    /// Drops flows with no traffic in both directions for `idle_timeout`, or in either
    /// direction for `one_way_timeout`. Returns how many were removed.
    pub fn evict_idle(
        &mut self,
        now: Instant,
        idle_timeout: Duration,
        one_way_timeout: Duration,
    ) -> usize {
        let by_client = &mut self.by_client;
        let before = self.flows.len();

        self.flows.retain(|_, flow| {
            let idle = flow.is_idle(now, idle_timeout, one_way_timeout);
            if idle {
                by_client.remove(&flow.client);
                tracing::debug!(client = ?flow.client, "evicting idle udp flow");
            }
            !idle
        });

        before - self.flows.len()
    }

    pub fn details(&self) -> Vec<UdpFlowDetails> {
        let now = Instant::now();
        self.flows
            .values()
            .map(|flow| UdpFlowDetails {
                client_addr: flow.client.client_addr,
                tunnel_id: flow.client.tunnel_id,
                port_offset: flow.client.port_offset,
                origin_addr: flow.target,
                local_addr: flow.socket.local_addr().ok(),
                age_ms: now.saturating_duration_since(flow.opened_at).as_millis() as u64,
                idle_from_tunnel_ms: now
                    .saturating_duration_since(flow.last_from_tunnel)
                    .as_millis() as u64,
                idle_from_origin_ms: now
                    .saturating_duration_since(flow.last_from_origin)
                    .as_millis() as u64,
            })
            .collect()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UdpFlowDetails {
    pub client_addr: SocketAddr,
    pub tunnel_id: u64,
    pub port_offset: u16,
    pub origin_addr: SocketAddr,
    pub local_addr: Option<SocketAddr>,
    pub age_ms: u64,
    pub idle_from_tunnel_ms: u64,
    pub idle_from_origin_ms: u64,
}

async fn receive_from_origin(
    key: FlowKey,
    socket: Arc<UdpSocket>,
    pool: PacketPool,
    origin_tx: mpsc::Sender<OriginPacket>,
    stats: AgentStats,
) {
    let mut next_error_log = Instant::now();

    loop {
        let mut packet = pool.allocate().await;

        match socket.recv_from(packet.capacity_mut()).await {
            Ok((len, from)) => {
                packet.set_len(len);
                let sent = origin_tx
                    .send(OriginPacket {
                        flow: key,
                        from,
                        packet,
                    })
                    .await;
                if sent.is_err() {
                    return;
                }
            }
            Err(error) => {
                stats.udp().recv_failed.inc();
                let now = Instant::now();
                if next_error_log <= now {
                    tracing::warn!(?error, ?key, "failed to receive from origin socket");
                    next_error_log = now + Duration::from_secs(1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use super::*;

    fn flow(port: u16) -> UdpFlow {
        UdpFlow::V4 {
            src: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), port),
            dst: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 1),
            frag: None,
            extension: None,
        }
    }

    #[tokio::test]
    async fn evicts_by_both_idle_or_one_way_idle() {
        let (tx, _rx) = mpsc::channel(4);
        let mut table = FlowTable::new(PacketPool::new(4), tx, AgentStats::new());
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let target = socket.local_addr().unwrap();
        let start = Instant::now();

        let mk = |port| ClientKey {
            client_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), port)),
            tunnel_id: 1,
            port_offset: 0,
        };

        let both_idle = table.insert(mk(1), socket.clone(), target, flow(1), start);
        let one_way = table.insert(mk(2), socket.clone(), target, flow(2), start);
        let active = table.insert(mk(3), socket.clone(), target, flow(3), start);

        let idle = Duration::from_secs(60);
        let one_way_limit = Duration::from_secs(90);
        let now = start + Duration::from_secs(70);

        table.get_mut(one_way).unwrap().last_from_tunnel = now;
        table.get_mut(active).unwrap().last_from_tunnel = now;
        table.get_mut(active).unwrap().last_from_origin = now;

        assert_eq!(table.evict_idle(now, idle, one_way_limit), 1);
        assert!(table.get(both_idle).is_none());
        assert!(table.find_mut(&mk(1)).is_none());
        assert!(table.get(one_way).is_some());

        let later = start + Duration::from_secs(95);
        table.get_mut(active).unwrap().last_from_tunnel = later;
        table.get_mut(active).unwrap().last_from_origin = later;
        assert_eq!(table.evict_idle(later, idle, one_way_limit), 1);
        assert!(table.get(one_way).is_none());
        assert_eq!(table.len(), 1);
    }

    #[tokio::test]
    async fn stale_keys_do_not_resolve_after_removal() {
        let (tx, _rx) = mpsc::channel(4);
        let mut table = FlowTable::new(PacketPool::new(4), tx, AgentStats::new());
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let target = socket.local_addr().unwrap();
        let client = ClientKey {
            client_addr: "1.1.1.1:5".parse().unwrap(),
            tunnel_id: 9,
            port_offset: 0,
        };

        let key = table.insert(client, socket.clone(), target, flow(5), Instant::now());
        assert!(table.remove(key).is_some());
        assert!(table.get(key).is_none());

        let replacement = table.insert(client, socket, target, flow(5), Instant::now());
        assert_ne!(key, replacement);
        assert!(table.get(key).is_none());
        assert!(table.get(replacement).is_some());
    }
}
