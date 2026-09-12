use super::{
    packets::{Packet, Packets},
    udp_errors::udp_errors,
    udp_receiver::{UdpReceivedPacket, UdpReceiver, UdpReceiverSetup},
    udp_settings::UdpSettings,
};
use crate::{
    network::{
        lan_address::LanAddress, origin_lookup::OriginLookup, proxy_protocol::ProxyProtocolHeader,
    },
    stats::AgentStats,
};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_agent_proto::udp_proto::UdpFlow;
use playit_api_client::api::ProxyProtocol;
use slotmap::{Key, KeyData, SlotMap};
use std::{collections::HashMap, net::SocketAddr, num::NonZeroU32, sync::Arc, time::Duration};
use tokio::{
    net::UdpSocket,
    sync::mpsc::{Receiver, channel},
    time::Instant,
};

slotmap::new_key_type! { struct ClientId; }

pub struct UdpClients {
    lookup: Arc<OriginLookup>,
    by_flow: HashMap<FlowKey, ClientId>,
    clients: SlotMap<ClientId, Client>,
    setup: UdpReceiverSetup,
    rx: Receiver<UdpReceivedPacket>,
    new_client_limiter: DefaultDirectRateLimiter,
    settings: UdpSettings,
    stats: AgentStats,
}

struct Client {
    key: FlowKey,
    socket: Arc<UdpSocket>,
    target: SocketAddr,
    reply: UdpFlow,
    receiver: UdpReceiver,
    last_activity: Instant,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct FlowKey {
    source: SocketAddr,
    destination: SocketAddr,
    tunnel_id: u64,
    port_offset: u16,
}

impl UdpClients {
    pub fn new(
        settings: UdpSettings,
        lookup: Arc<OriginLookup>,
        packets: Packets,
        stats: AgentStats,
    ) -> Self {
        let (output, rx) = channel(2048);
        let quota = Quota::per_second(
            NonZeroU32::new(settings.new_client_ratelimit).unwrap_or(NonZeroU32::MIN),
        )
        .allow_burst(
            NonZeroU32::new(settings.new_client_ratelimit_burst).unwrap_or(NonZeroU32::MIN),
        );
        Self {
            lookup,
            by_flow: HashMap::new(),
            clients: SlotMap::with_key(),
            setup: UdpReceiverSetup { output, packets },
            rx,
            new_client_limiter: RateLimiter::direct(quota),
            settings,
            stats,
        }
    }

    pub async fn clear_old(&mut self) {
        let expired: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, client)| {
                client.receiver.is_closed()
                    || client.last_activity.elapsed() >= self.settings.idle_timeout
            })
            .map(|(id, _)| id)
            .collect();
        for id in expired {
            if let Some(client) = self.clients.remove(id) {
                self.by_flow.remove(&client.key);
                client.receiver.shutdown().await;
            }
        }
        self.stats.set_udp(self.clients.len() as u32);
    }

    pub async fn recv_origin_packet(&mut self) -> UdpReceivedPacket {
        self.rx.recv().await.expect("UDP clients own a sender")
    }

    pub async fn dispatch_origin_packet(
        &mut self,
        packet: UdpReceivedPacket,
    ) -> Option<(UdpFlow, Packet)> {
        let id = ClientId::from(KeyData::from_ffi(packet.rx_id));
        let Some(client) = self.clients.get_mut(id) else {
            udp_errors().origin_client_missing.inc();
            return None;
        };
        if packet.from != client.target {
            udp_errors().origin_reject_addr_differ.inc();
            return None;
        }
        client.last_activity = Instant::now();
        self.stats.add_bytes_out(packet.packet.len() as u64);
        Some((client.reply, packet.packet))
    }

    pub async fn handle_tunneled_packet(&mut self, flow: UdpFlow, packet: Packet) {
        // Fragment reassembly is unsupported; fragments must never reach an origin as complete datagrams.
        if matches!(flow, UdpFlow::V4 { frag: Some(_), .. }) {
            udp_errors().unsupported_fragment.inc();
            return;
        }
        let Some(extension) = flow.extension() else {
            return;
        };
        let Some(origin) = self.lookup.lookup(extension.tunnel_id.get(), false).await else {
            return;
        };
        let key = FlowKey {
            source: flow.src(),
            destination: flow.dst(),
            tunnel_id: extension.tunnel_id.get(),
            port_offset: extension.port_offset,
        };
        let id = match self.by_flow.get(&key).copied() {
            Some(id) if !self.clients[id].receiver.is_closed() => id,
            old => {
                if let Some(id) = old {
                    self.by_flow.remove(&key);
                    self.clients.remove(id);
                    self.stats.set_udp(self.clients.len() as u32);
                }
                if self.clients.len() >= self.settings.max_clients
                    || self.new_client_limiter.check().is_err()
                {
                    udp_errors().new_client_ratelimit.inc();
                    return;
                }
                let Ok(Some(target)) = tokio::time::timeout(
                    Duration::from_secs(2),
                    origin.resolve_local(extension.port_offset),
                )
                .await
                else {
                    return;
                };
                let special_lan = target.ip().is_loopback()
                    && target.is_ipv4()
                    && origin.proxy_protocol.is_none();
                let socket =
                    match LanAddress::udp_socket(special_lan, key.source, target, key.tunnel_id)
                        .await
                    {
                        Ok(socket) => Arc::new(socket),
                        Err(error) => {
                            tracing::debug!(?error, %target, "UDP origin socket failed");
                            return;
                        }
                    };
                if let Some(proto) = origin.proxy_protocol {
                    if proto != ProxyProtocol::ProxyProtocolV2 {
                        udp_errors().origin_v1_proxy_protocol.inc();
                    } else {
                        let mut header = Vec::new();
                        ProxyProtocolHeader::from_udp_flow(&flow)
                            .write_v2_udp(&mut header)
                            .unwrap();
                        if socket.send_to(&header, target).await.is_err() {
                            udp_errors().origin_send_io_error.inc();
                            return;
                        }
                    }
                }
                let id = self.clients.insert_with_key(|id| Client {
                    key: key.clone(),
                    receiver: self.setup.create(id.data().as_ffi(), socket.clone()),
                    socket,
                    target,
                    reply: flow.flip(),
                    last_activity: Instant::now(),
                });
                self.by_flow.insert(key, id);
                self.stats.set_udp(self.clients.len() as u32);
                id
            }
        };
        let client = &mut self.clients[id];
        client.reply = flow.flip();
        if client
            .socket
            .send_to(packet.as_ref(), client.target)
            .await
            .is_err()
        {
            udp_errors().origin_send_io_error.inc();
            return;
        }
        client.last_activity = Instant::now();
        self.stats.add_bytes_in(packet.len() as u64);
    }
}

impl Drop for UdpClients {
    fn drop(&mut self) {
        self.stats.set_udp(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::origin_lookup::{OriginIp, OriginResource, OriginTarget};
    use playit_agent_proto::{PortProto, udp_proto::UdpFlowExtension};
    use std::num::NonZeroU64;

    fn flow(port_offset: u16) -> UdpFlow {
        UdpFlow::V4 {
            src: "198.51.100.1:4000".parse().unwrap(),
            dst: format!("203.0.113.1:{}", 5000 + port_offset)
                .parse()
                .unwrap(),
            frag: None,
            extension: Some(UdpFlowExtension {
                tunnel_id: NonZeroU64::new(42).unwrap(),
                client_server_id: NonZeroU64::new(7).unwrap(),
                port_offset,
            }),
        }
    }
    fn packet(pool: &Packets, bytes: &[u8]) -> Packet {
        let mut packet = pool.allocate().unwrap();
        packet.set_len(bytes.len()).unwrap();
        packet.as_mut().copy_from_slice(bytes);
        packet
    }
    async fn receive(socket: &UdpSocket) -> SocketAddr {
        let mut buffer = [0; 64];
        let (len, source) =
            tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut buffer))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(&buffer[..len], b"request");
        source
    }

    #[tokio::test]
    async fn ports_are_isolated_and_expired_receiver_ids_cannot_reach_new_clients() {
        let origin0 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let origin1 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let lookup = Arc::new(OriginLookup::default());
        lookup
            .update(std::iter::once(OriginResource {
                tunnel_id: 42,
                proto: PortProto::Udp,
                port_count: 2,
                proxy_protocol: None,
                target: OriginTarget::Https {
                    ip: OriginIp::IpAddress("127.0.0.1".parse().unwrap()),
                    http_port: origin0.local_addr().unwrap().port(),
                    https_port: origin1.local_addr().unwrap().port(),
                },
            }))
            .await;
        let pool = Packets::new(16);
        let stats = AgentStats::new();
        let mut clients =
            UdpClients::new(UdpSettings::default(), lookup, pool.clone(), stats.clone());
        clients
            .handle_tunneled_packet(flow(0), packet(&pool, b"request"))
            .await;
        let peer0 = receive(&origin0).await;
        clients
            .handle_tunneled_packet(flow(1), packet(&pool, b"request"))
            .await;
        let peer1 = receive(&origin1).await;
        assert_ne!(peer0, peer1);
        assert_eq!(stats.active_udp(), 2);
        origin1.send_to(b"reply", peer1).await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(1), clients.recv_origin_packet())
            .await
            .unwrap();
        let (reply_flow, _) = clients.dispatch_origin_packet(reply).await.unwrap();
        assert_eq!(reply_flow, flow(1).flip());
        origin0.send_to(b"late", peer0).await.unwrap();
        let late = tokio::time::timeout(Duration::from_secs(1), clients.recv_origin_packet())
            .await
            .unwrap();
        for client in clients.clients.values_mut() {
            client.last_activity -= Duration::from_secs(100);
        }
        clients.clear_old().await;
        clients
            .handle_tunneled_packet(flow(0), packet(&pool, b"request"))
            .await;
        receive(&origin0).await;
        assert_eq!(stats.active_udp(), 1);
        assert!(clients.dispatch_origin_packet(late).await.is_none());
        drop(clients);
        assert_eq!(stats.active_udp(), 0);
    }
}
