use std::{
    collections::HashMap,
    net::{SocketAddr, SocketAddrV4},
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_api_client::api::ProxyProtocol;
use slab::Slab;
use tokio::{
    net::UdpSocket,
    sync::mpsc::{Receiver, channel},
};

use crate::network::{
    lan_address::LanAddress, origin_lookup::OriginLookup, proxy_protocol::ProxyProtocolHeader,
};
use crate::stats::AgentStats;
use playit_agent_proto::udp_proto::UdpFlow;

use super::{
    packets::{Packet, Packets},
    udp_errors::udp_errors,
    udp_receiver::{UdpReceivedPacket, UdpReceiver, UdpReceiverSetup},
    udp_settings::UdpSettings,
};

pub struct UdpClients {
    lookup: Arc<OriginLookup>,

    virtual_client_lookup: HashMap<UdpClientKey, usize>,
    virtual_clients: Slab<Client>,
    next_client_generation: AtomicU32,
    setup: UdpReceiverSetup,
    rx: Receiver<UdpReceivedPacket>,

    new_client_limiter: DefaultDirectRateLimiter,
    stats: AgentStats,
}

struct Client {
    id: u64,
    key: UdpClientKey,
    socket: Arc<UdpSocket>,
    target_addr: SocketAddr,
    port_offset: u16,
    flow: UdpFlow,

    /* when dropped, rx task get killed */
    receiver: UdpReceiver,

    from_tunnel_ts: u64,
    from_origin_ts: u64,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct UdpClientKey {
    source_addr: SocketAddr,
    tunnel_id: u64,
}

impl UdpClientKey {
    pub async fn create_socket(
        &self,
        special_lan: bool,
        target_addr: SocketAddr,
    ) -> std::io::Result<UdpSocket> {
        LanAddress::udp_socket(special_lan, self.source_addr, target_addr, self.tunnel_id).await
    }
}

fn build_quota(settings: &UdpSettings) -> Quota {
    let rate = NonZeroU32::new(settings.new_client_ratelimit).unwrap_or_else(|| {
        tracing::warn!("invalid udp new client rate limit of 0, clamping to 1");
        NonZeroU32::MIN
    });
    let burst = NonZeroU32::new(settings.new_client_ratelimit_burst).unwrap_or_else(|| {
        tracing::warn!("invalid udp new client burst of 0, clamping to 1");
        NonZeroU32::MIN
    });

    Quota::per_second(rate).allow_burst(burst)
}

fn pack_client_id(slot: u32, generation: u32) -> u64 {
    ((generation as u64) << 32) | (slot as u64)
}

fn unpack_slot(id: u64) -> usize {
    (id as u32) as usize
}

impl UdpClients {
    pub fn new(
        settings: UdpSettings,
        lookup: Arc<OriginLookup>,
        packets: Packets,
        stats: AgentStats,
    ) -> Self {
        let (origin_tx, origin_rx) = channel(2048);

        UdpClients {
            lookup,
            virtual_client_lookup: HashMap::new(),
            virtual_clients: Slab::with_capacity(2048),
            next_client_generation: AtomicU32::new(1),
            setup: UdpReceiverSetup {
                output: origin_tx,
                packets,
            },
            rx: origin_rx,
            new_client_limiter: RateLimiter::direct(build_quota(&settings)),
            stats,
        }
    }

    pub async fn clear_old(&mut self, now_ms: u64) {
        let mut to_remove = Vec::new();

        for (slot, client) in self.virtual_clients.iter_mut() {
            let receiver_closed = client.receiver.is_closed();
            let since_origin = now_ms.saturating_sub(client.from_origin_ts);
            let since_tunnel = now_ms.saturating_sub(client.from_tunnel_ts);

            let remove = {
                receiver_closed
                ||
                /* both haven't seen action in over 1m */
                60_000 < since_tunnel && 60_000 < since_origin
                /* either side has no traffic in 1.5m */
                || 90_000 < since_tunnel
                || 90_000 < since_origin
            };

            if remove {
                to_remove.push(slot);
            }
        }

        to_remove.sort_unstable();
        to_remove.reverse();

        for slot in to_remove {
            let client = self.virtual_clients.remove(slot);
            let removed = self.virtual_client_lookup.remove(&client.key).unwrap();
            assert_eq!(removed, slot);
            client.receiver.shutdown().await;
        }

        // Update active UDP count
        self.stats.set_udp(self.virtual_clients.len() as u32);
    }

    pub async fn recv_origin_packet(&mut self) -> UdpReceivedPacket {
        self.rx
            .recv()
            .await
            .expect("should never close with local reference")
    }

    pub async fn dispatch_origin_packet(
        &mut self,
        now_ms: u64,
        packet: UdpReceivedPacket,
    ) -> Option<(UdpFlow, Packet)> {
        let Some(client) = self.virtual_clients.get_mut(unpack_slot(packet.rx_id)) else {
            udp_errors().origin_client_missing.inc();
            return None;
        };

        if client.id != packet.rx_id {
            udp_errors().origin_reject_bad_id.inc();
            return None;
        }

        if packet.from != client.target_addr {
            udp_errors().origin_reject_addr_differ.inc();
            return None;
        }

        let port_offset = client.port_offset;

        client.from_origin_ts = now_ms;

        // Track bytes going out (from origin to tunnel)
        let packet_len = packet.packet.len() as u64;
        self.stats.add_bytes_out(packet_len);

        let mut flow = client.flow;
        match &mut flow {
            UdpFlow::V4 {
                src,
                extension: Some(ext),
                ..
            } => {
                *src = SocketAddrV4::new(*src.ip(), src.port() + port_offset);
                ext.port_offset = port_offset;
            }
            UdpFlow::V6 {
                src,
                extension: Some(ext),
                ..
            } => {
                src.1 += port_offset;
                ext.port_offset = port_offset;
            }
            _ => unreachable!(),
        }

        Some((flow, packet.packet))
    }

    pub async fn handle_tunneled_packet(&mut self, now_ms: u64, flow: UdpFlow, packet: Packet) {
        let Some(extension) = flow.extension() else {
            return;
        };
        let Some(origin) = self.lookup.lookup(extension.tunnel_id.get(), false).await else {
            return;
        };

        let key = UdpClientKey {
            source_addr: flow.src(),
            tunnel_id: extension.tunnel_id.get(),
        };

        // Track bytes coming in (from tunnel to origin)
        let packet_len = packet.len() as u64;

        if let Some(&slot) = self.virtual_client_lookup.get(&key) {
            let receiver_closed = self
                .virtual_clients
                .get_mut(slot)
                .map(|client| client.receiver.is_closed())
                .unwrap_or(true);

            if !receiver_closed {
                let client = self.virtual_clients.get_mut(slot).unwrap();

                client.from_tunnel_ts = now_ms;
                if client
                    .socket
                    .send_to(packet.as_ref(), client.target_addr)
                    .await
                    .is_err()
                {
                    udp_errors().origin_send_io_error.inc();
                }

                self.stats.add_bytes_in(packet_len);
                return;
            }

            self.virtual_client_lookup.remove(&key);
            if self.virtual_clients.get(slot).is_some() {
                self.virtual_clients.remove(slot);
            }
            self.stats.set_udp(self.virtual_clients.len() as u32);
        }

        if self.new_client_limiter.check().is_err() {
            udp_errors().new_client_ratelimit.inc();
            return;
        }

        let Some(target_addr) = origin.resolve_local(extension.port_offset).await else {
            return;
        };

        let special_lan = matches!(target_addr, SocketAddr::V4(addr) if addr.ip().is_loopback())
            && origin.proxy_protocol.is_none();

        let socket = match key.create_socket(special_lan, target_addr).await {
            Ok(socket) => Arc::new(socket),
            Err(error) => {
                tracing::error!(
                    ?error,
                    target_addr = %target_addr,
                    source_addr = %key.source_addr,
                    tunnel_id = key.tunnel_id,
                    "failed to open local UDP socket for tunnel traffic"
                );
                return;
            }
        };

        let entry = self.virtual_clients.vacant_entry();
        let slot = entry.key();
        let slot = u32::try_from(slot).expect("udp client slot overflow");
        let generation = self.next_client_generation.fetch_add(1, Ordering::Relaxed);
        let id = pack_client_id(slot, generation);

        let receiver = self.setup.create(id, socket.clone());

        let mut client_flow = flow.flip();
        match &mut client_flow {
            UdpFlow::V4 {
                src,
                extension: Some(ext),
                ..
            } => {
                if extension.port_offset != 0 {
                    assert!(extension.port_offset <= src.port());
                    *src = SocketAddrV4::new(*src.ip(), src.port() - extension.port_offset);
                }
                ext.port_offset = 0;
            }
            UdpFlow::V6 {
                src,
                extension: Some(ext),
                ..
            } => {
                if extension.port_offset != 0 {
                    assert!(extension.port_offset <= src.1);
                    src.1 -= extension.port_offset;
                }
                ext.port_offset = 0;
            }
            _ => unreachable!(),
        }

        let client = Client {
            id,
            key: key.clone(),
            socket,
            target_addr,
            port_offset: extension.port_offset,
            receiver,
            flow: client_flow,
            from_tunnel_ts: now_ms,
            from_origin_ts: now_ms,
        };

        if let Some(proto) = origin.proxy_protocol {
            if proto != ProxyProtocol::ProxyProtocolV2 {
                udp_errors().origin_v1_proxy_protocol.inc();
            } else {
                let header = ProxyProtocolHeader::from_udp_flow(&flow);

                let mut buffer = Vec::new();
                header
                    .write_v2_udp(&mut buffer)
                    .expect("Failed to write proxy proto header to Vec");

                if client.socket.send_to(&buffer, target_addr).await.is_err() {
                    udp_errors().origin_send_io_error.inc();
                }
            }
        }

        if client
            .socket
            .send_to(packet.as_ref(), target_addr)
            .await
            .is_err()
        {
            udp_errors().origin_send_io_error.inc();
        }

        self.virtual_client_lookup.insert(
            key,
            usize::try_from(slot).expect("udp client slot overflow"),
        );
        entry.insert(client);

        // Update active UDP count for new client
        self.stats.set_udp(self.virtual_clients.len() as u32);
    }
}
