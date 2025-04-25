use std::{collections::{hash_map, HashMap}, net::{IpAddr, SocketAddr, SocketAddrV4}, num::NonZeroU32, sync::Arc};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_api_client::api::ProxyProtocol;
use slab::Slab;
use tokio::{net::UdpSocket, sync::mpsc::{channel, Receiver}};

use crate::network::{lan_address::LanAddress, origin_lookup::OriginLookup, proxy_protocol::ProxyProtocolHeader};
use playit_agent_proto::udp_proto::UdpFlow;

use super::{packets::{Packet, Packets}, udp_errors::udp_errors, udp_receiver::{UdpReceivedPacket, UdpReceiver, UdpReceiverSetup}, udp_settings::UdpSettings};

pub struct UdpClients {
    lookup: Arc<OriginLookup>,

    virtual_client_lookup: HashMap<UdpClientKey, usize>,
    virtual_clients: Slab<Client>,
    setup: UdpReceiverSetup,
    rx: Receiver<UdpReceivedPacket>,

    new_client_limiter: DefaultDirectRateLimiter,
}

struct Client {
    id: u64,
    key: UdpClientKey,
    socket: Arc<UdpSocket>,
    flow: UdpFlow,

    /* when dropped, rx task get killed */
    _receiver: UdpReceiver,

    from_tunnel_ts: u64,
    from_origin_ts: u64,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct UdpClientKey {
    source_addr: SocketAddr,
    tunnel_id: u64,
}

impl UdpClientKey {
    pub async fn create_socket(&self, special_lan: bool) -> std::io::Result<UdpSocket> {
        LanAddress::udp_socket(special_lan, self.source_addr, self.tunnel_id).await
    }
}

impl UdpClients {
    pub fn new(settings: UdpSettings, lookup: Arc<OriginLookup>, packets: Packets) -> Self {
        let (origin_tx, origin_rx) = channel(2048);

        let quota = unsafe {
            Quota::per_second(NonZeroU32::new_unchecked(settings.new_client_ratelimit))
                .allow_burst(NonZeroU32::new_unchecked(settings.new_client_ratelimit_burst))
        };

        UdpClients {
            lookup,
            virtual_client_lookup: HashMap::new(),
            virtual_clients: Slab::with_capacity(2048),
            setup: UdpReceiverSetup {
                output: origin_tx,
                packets,
            },
            rx: origin_rx,
            new_client_limiter: RateLimiter::direct(quota),
        }
    }

    pub fn clear_old(&mut self, now_ms: u64) {
        self.virtual_clients.retain(|slot, client| {
            let since_origin = now_ms - client.from_origin_ts;
            let since_tunnel = now_ms - client.from_tunnel_ts;

            let remove = {
                /* both haven't seen action in over 1m */
                60_000 < since_tunnel && 60_000 < since_origin
                /* either side has no traffic in 1.5m */
                || 90_000 < since_tunnel
                || 90_000 < since_origin
            };

            if remove {
                let removed = self.virtual_client_lookup.remove(&client.key).unwrap();
                assert_eq!(removed, slot);

                false
            } else {
                true
            }
        });
    }

    pub async fn recv_origin_packet(&mut self) -> UdpReceivedPacket {
        self.rx.recv().await.expect("should never close with local reference")
    }

    pub async fn dispatch_origin_packet(&mut self, now_ms: u64, packet: UdpReceivedPacket) -> Option<(UdpFlow, Packet)> {
        let Some(client) = self.virtual_clients.get_mut((packet.rx_id as u32) as usize) else {
            udp_errors().origin_client_missing.inc();
            return None;
        };

        if client.id != packet.rx_id {
            udp_errors().origin_reject_bad_id.inc();
            return None;
        }

        let Some(tunnel) = self.lookup.lookup(client.key.tunnel_id, false).await else {
            udp_errors().origin_tunnel_not_found.inc();
            return None;
        };

        let SocketAddr::V4(source) = packet.from else {
            udp_errors().origin_source_not_ip4.inc();
            return None
        };

        if tunnel.local_addr.ip() != IpAddr::V4(*source.ip()) {
            udp_errors().origin_reject_addr_differ.inc();
            return None;
        }

        if source.port() < tunnel.local_addr.port() {
            udp_errors().origin_reject_port_too_low.inc();
            return None;
        }

        let port_offset = source.port() - tunnel.local_addr.port();
        if tunnel.port_count <= port_offset {
            udp_errors().origin_reject_port_too_high.inc();
            return None;
        }

        client.from_origin_ts = now_ms;

        let mut flow = client.flow;
        match &mut flow {
            UdpFlow::V4 { src, extension: Some(ext), .. } => {
                *src = SocketAddrV4::new(*src.ip(), src.port() + port_offset);
                ext.port_offset = port_offset;
            }
            UdpFlow::V6 { src, extension: Some(ext), .. } => {
                src.1 += port_offset;
                ext.port_offset = port_offset;
            }
            _ => unreachable!(),
        }

        Some((flow, packet.packet))
    }

    pub async fn handle_tunneled_packet(&mut self, now_ms: u64, flow: UdpFlow, packet: Packet) {
        let Some(extension) = flow.extension() else { return };
        let Some(origin) = self.lookup.lookup(extension.tunnel_id.get(), false).await else { return };

        let key = UdpClientKey {
            source_addr: flow.src(),
            tunnel_id: extension.tunnel_id.get(),
        };

        let target_addr = if extension.port_offset == 0 {
            let SocketAddr::V4(addr) = origin.local_addr else { return };
            addr
        } else {
            let IpAddr::V4(ip) = origin.local_addr.ip() else { return };
            SocketAddrV4::new(ip, origin.local_addr.port() + extension.port_offset)
        };

        match self.virtual_client_lookup.entry(key) {
            hash_map::Entry::Occupied(o) => {
                let slot = *o.get();

                let client = self.virtual_clients
                    .get_mut(slot).unwrap();

                client.from_tunnel_ts = now_ms;
                if client.socket.send_to(packet.as_ref(), target_addr).await.is_err() {
                    udp_errors().origin_send_io_error.inc();
                }
            }
            hash_map::Entry::Vacant(v) => {
                if self.new_client_limiter.check().is_err() {
                    udp_errors().new_client_ratelimit.inc();
                    return;
                }

                let special_lan = origin.local_addr.ip().is_loopback() && origin.proxy_protocol.is_none();
                let socket = match v.key().create_socket(special_lan).await {
                    Ok(socket) => Arc::new(socket),
                    Err(error) => {
                        tracing::error!(?error, "failed to create socket");
                        return;
                    }
                };

                let entry = self.virtual_clients.vacant_entry();
                let slot = entry.key();
                let id = slot as u64;

                let receiver = self.setup.create(
                    id,
                    socket.clone(),
                );

                let mut client_flow = flow.flip();
                match &mut client_flow {
                    UdpFlow::V4 { src, extension: Some(ext), .. } => {
                        if extension.port_offset != 0 {
                            assert!(extension.port_offset <= src.port());
                            *src = SocketAddrV4::new(*src.ip(), src.port() - extension.port_offset);
                        }
                        ext.port_offset = 0;
                    }
                    UdpFlow::V6 { src, extension: Some(ext), .. } => {
                        if extension.port_offset != 0 {
                            assert!(extension.port_offset <= src.1);
                            src.1 -= extension.port_offset;
                        }
                        ext.port_offset = 0;
                    }
                    _ => unreachable!()
                }

                let key = v.key().clone();

                let client = Client {
                    id,
                    key,
                    socket,
                    _receiver: receiver,
                    flow: client_flow,
                    from_tunnel_ts: now_ms,
                    from_origin_ts: now_ms,
                };

                if let Some(proto) = origin.proxy_protocol {
                    if proto != ProxyProtocol::ProxyProtocolV2 {
                        udp_errors().origin_v1_proxy_protocol.inc();
                    } else {
                        let header = ProxyProtocolHeader::from_udp_flow(&client_flow);

                        let mut buffer = Vec::new();
                        header.write_v2_udp(&mut buffer).expect("Failed to write proxy proto header to Vec");

                        if client.socket.send_to(&buffer, target_addr).await.is_err() {
                            udp_errors().origin_send_io_error.inc();
                        }
                    }
                }

                if client.socket.send_to(packet.as_ref(), target_addr).await.is_err() {
                    udp_errors().origin_send_io_error.inc();
                }

                v.insert(slot);
                entry.insert(client);
            }
        }
    }
}

