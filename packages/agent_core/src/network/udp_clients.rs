use std::collections::{hash_map::Entry, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use byteorder::{BigEndian, ByteOrder};
use hyper::Client;
use playit_agent_proto::PortProto;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use crate::network::address_lookup::{AddressLookup, MatchAddress};
use crate::network::lan_address::LanAddress;
use crate::tunnel::udp_proto::UdpFlow;
use crate::tunnel::udp_tunnel::UdpTunnel;
use crate::utils::now_milli;

pub struct UdpClients<L: AddressLookup> {
    udp_tunnel: UdpTunnel,
    lookup: L,
    udp_clients: Arc<RwLock<HashMap<ClientKey, Arc<UdpClient>>>>,
    pub use_special_lan: bool,
}

#[derive(Eq, PartialEq, Hash, Debug, Clone)]
struct ClientKey {
    client_addr: SocketAddr,
    tunnel_addr: SocketAddr,
}

impl<L: AddressLookup> UdpClients<L> {
    pub fn new(tunnel: UdpTunnel, lookup: L) -> Self {
        UdpClients {
            udp_tunnel: tunnel,
            lookup,
            udp_clients: Default::default(),
            use_special_lan: true,
        }
    }

    pub async fn client_count(&self) -> usize {
        let clients_lock = self.udp_clients.read().await;
        clients_lock.len()
    }

    pub async fn forward_packet(&self, flow: &UdpFlow, data: &[u8]) -> std::io::Result<usize> {
        let flow_dst = flow.dst();
        let match_addr = match self.lookup.tunnel_match_address(flow_dst, PortProto::Udp) {
            Some(v) => v,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "could not find tunnel",
                ))
            }
        };

        /* normalize port */
        let key = ClientKey {
            client_addr: flow.src(),
            tunnel_addr: SocketAddr::new(flow.dst().ip(), match_addr.from_port),
        };

        {
            let clients = self.udp_clients.read().await;

            if let Some(client) = clients.get(&key) {
                return client.send_local(flow_dst.port(), data).await;
            }
        }

        {
            let mut clients = self.udp_clients.write().await;
            let client = match clients.entry(key) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(v) => {
                    let local_addr = match self.lookup.local_address(match_addr, PortProto::Udp) {
                        Some(v) => v,
                        None => {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::ConnectionRefused,
                                "could not find tunnel",
                            ))
                        }
                    };

                    let (send_flow, client_addr) = match flow {
                        UdpFlow::V4 { src, dst } => (
                            UdpFlow::V4 {
                                src: SocketAddrV4::new(*dst.ip(), match_addr.from_port),
                                dst: *src,
                            },
                            SocketAddr::V4(SocketAddrV4::new(*src.ip(), src.port())),
                        ),
                        UdpFlow::V6 { src, dst, flow } => (
                            UdpFlow::V6 {
                                src: (dst.0, match_addr.from_port),
                                dst: *src,
                                flow: *flow,
                            },
                            SocketAddr::V6(SocketAddrV6::new(src.0, src.1, 0, 0)),
                        ),
                    };

                    let client_key = v.key().clone();
                    tracing::info!(?client_key, "setup new udp client");

                    let client = Arc::new(UdpClient {
                        client_key,
                        send_flow,
                        local_udp: LanAddress::udp_socket(self.use_special_lan, client_addr, local_addr).await?,
                        udp_tunnel: self.udp_tunnel.clone(),
                        local_start_addr: local_addr,
                        tunnel_from_port: match_addr.from_port,
                        tunnel_to_port: match_addr.to_port,
                        udp_clients: self.udp_clients.clone(),
                        last_activity: Default::default()
                    });

                    tokio::spawn(HostToTunnelForwarder(client.clone()).run());
                    v.insert(client)
                }
            };

            client.send_local(flow_dst.port(), data).await
        }
    }
}

struct UdpClient {
    client_key: ClientKey,
    send_flow: UdpFlow,
    local_udp: UdpSocket,
    udp_tunnel: UdpTunnel,
    local_start_addr: SocketAddr,
    tunnel_from_port: u16,
    tunnel_to_port: u16,
    udp_clients: Arc<RwLock<HashMap<ClientKey, Arc<UdpClient>>>>,
    last_activity: AtomicU64,
}

impl UdpClient {
    pub async fn send_local(&self, dst_port: u16, data: &[u8]) -> std::io::Result<usize> {
        let port_offset = dst_port - self.tunnel_from_port;

        let target_addr = if port_offset == 0 {
            self.local_start_addr
        } else {
            SocketAddr::new(
                self.local_start_addr.ip(),
                self.local_start_addr.port() + port_offset,
            )
        };

        self.last_activity.store(now_milli(), Ordering::Relaxed);
        self.local_udp.send_to(data, target_addr).await
    }
}

struct HostToTunnelForwarder(Arc<UdpClient>);

impl HostToTunnelForwarder {
    pub async fn run(self) {
        let mut buffer = vec![0u8; 2048];

        loop {
            tokio::task::yield_now().await;

            buffer.resize(2048, 0);
            let recv_res = tokio::time::timeout(
                Duration::from_secs(30),
                self.0.local_udp.recv_from(&mut buffer),
            )
            .await;

            let (bytes, source) = match recv_res {
                Ok(Ok(v)) => v,
                Ok(Err(error)) => {
                    tracing::error!(?error, "failed to receive data from host socket");
                    break;
                }
                Err(_) => {
                    if now_milli() - self.0.last_activity.load(Ordering::Relaxed) > 120_000 {
                        tracing::info!("2 min timeout for not receiving data from host");
                        break;
                    }
                    continue;
                }
            };

            // tracing::info!(bytes, %source, "got client packet");

            if source.ip() != self.0.local_start_addr.ip() {
                tracing::warn!(
                    source = %source.ip(),
                    expected = %self.0.local_start_addr.ip(),
                    "dropping packet from different unexpected source"
                );
                continue;
            }

            let port_count = self.0.tunnel_to_port - self.0.tunnel_from_port;
            let local_from = self.0.local_start_addr.port();
            let local_to = self.0.local_start_addr.port() + port_count;

            if source.port() < local_from || local_to <= source.port() {
                tracing::warn!(
                    source = source.port(),
                    from = local_from,
                    to = local_to,
                    "dropping packet outside of expected port range"
                );

                continue;
            }

            buffer.truncate(bytes);
            let port_offset = source.port() - local_from;

            let flow = self.0.send_flow.with_src_port(self.0.tunnel_from_port + port_offset);
            if let Err(error) = self.0.udp_tunnel.send(&mut buffer, flow).await {
                tracing::error!(?error, "failed to send packet to through tunnel");
            }
        }

        let removed = {
            let mut clients = self.0.udp_clients.write().await;
            clients.remove(&self.0.client_key)
        };

        match removed {
            None => {
                tracing::error!("could not find UDP client when removing");
            }
            Some(v) if !Arc::ptr_eq(&v, &self.0) => {
                tracing::error!("removing different UDP client when closing");
            }
            _ => {
                tracing::info!(flow = ?self.0.send_flow, "udp client removed");
            }
        }
    }
}
