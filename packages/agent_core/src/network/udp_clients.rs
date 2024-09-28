use std::collections::{hash_map::Entry, HashMap};
use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use crate::agent_control::PacketIO;
use playit_api_client::api::{PortType, ProxyProtocol};
use crate::network::address_lookup::AddressLookup;
use crate::network::lan_address::LanAddress;
use crate::agent_control::udp_proto::UdpFlow;
use crate::agent_control::udp_channel::UdpChannel;
use crate::utils::now_sec;

use super::address_lookup::HostOrigin;
use super::proxy_protocol::{ProxyProtocolHeader, UDP_PROXY_PROTOCOL_LEN_V4, UDP_PROXY_PROTOCOL_LEN_V6};

pub struct UdpClients<L: AddressLookup, I: PacketIO> {
    udp_tunnel: UdpChannel<I>,
    lookup: L,
    udp_clients: Arc<RwLock<HashMap<ClientKey, Arc<UdpClient<I>>>>>,
    pub use_special_lan: bool,
}

#[derive(Eq, PartialEq, Hash, Debug, Clone)]
struct ClientKey {
    client_addr: SocketAddr,
    tunnel_addr: SocketAddr,
}

impl<L: AddressLookup, I: PacketIO> UdpClients<L, I> where L::Value: Into<HostOrigin> {
    pub fn new(tunnel: UdpChannel, lookup: L) -> Self {
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

    pub async fn handle_tunnel_packet(&self, flow: &UdpFlow, data: &mut [u8], data_offset: usize) -> std::io::Result<usize> {
        let flow_dst = flow.dst();

        let found = self.lookup.lookup(flow_dst.ip(), flow_dst.port(), PortType::Udp)
            .ok_or(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "could not find tunnel"))?;

        /* normalize port to share same UDP socket from same client */
        let key = ClientKey {
            client_addr: flow.src(),
            tunnel_addr: SocketAddr::new(flow_dst.ip(), found.from_port),
        };

        {
            let clients = self.udp_clients.read().await;

            if let Some(client) = clients.get(&key) {
                return client_send(flow, client, data, data_offset).await;
            }
        }

        {
            let mut clients = self.udp_clients.write().await;

            let client = match clients.entry(key) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(v) => {
                    let origin: HostOrigin = found.value.into();

                    let (send_flow, client_addr) = match flow {
                        UdpFlow::V4 { src, dst } => (
                            UdpFlow::V4 {
                                src: SocketAddrV4::new(*dst.ip(), found.from_port),
                                dst: *src,
                            },
                            SocketAddr::V4(SocketAddrV4::new(*src.ip(), src.port())),
                        ),
                        UdpFlow::V6 { src, dst, flow } => (
                            UdpFlow::V6 {
                                src: (dst.0, found.from_port),
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
                        local_udp: LanAddress::udp_socket(self.use_special_lan, client_addr, origin.host_addr).await?,
                        udp_tunnel: self.udp_tunnel.clone(),
                        local_start_addr: origin.host_addr,
                        tunnel_from_port: found.from_port,
                        tunnel_to_port: found.to_port,
                        udp_clients: self.udp_clients.clone(),
                        last_activity: Default::default(),
                        write_proxy_protocol_header: origin.proxy_protocol == Some(ProxyProtocol::ProxyProtocolV2),
                    });

                    tokio::spawn(HostToTunnelForwarder(client.clone()).run());
                    v.insert(client)
                }
            };

            return client_send(flow, client, data, data_offset).await;
        }
    }
}

struct UdpClient<I: PacketIO> {
    client_key: ClientKey,
    send_flow: UdpFlow,
    local_udp: UdpSocket,
    udp_tunnel: UdpChannel<I>,
    local_start_addr: SocketAddr,
    tunnel_from_port: u16,
    tunnel_to_port: u16,
    udp_clients: Arc<RwLock<HashMap<ClientKey, Arc<UdpClient<I>>>>>,
    last_activity: AtomicU32,
    write_proxy_protocol_header: bool,
}

impl<I: PacketIO> UdpClient<I> {
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

        self.last_activity.store(now_sec(), Ordering::Relaxed);
        self.local_udp.send_to(data, target_addr).await
    }
}

struct HostToTunnelForwarder<I: PacketIO>(Arc<UdpClient<I>>);

impl<I: PacketIO> HostToTunnelForwarder<I> {
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
                    if now_sec() - self.0.last_activity.load(Ordering::Relaxed) > 120 {
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
            Some(client) => {
                tracing::info!(flow = ?self.0.send_flow, key = ?client.client_key, "udp client removed");
            }
        }
    }
}

async fn client_send<I: PacketIO>(flow: &UdpFlow, client: &UdpClient<I>, data: &mut [u8], data_offset: usize) -> std::io::Result<usize> {
    let flow_dst_port = flow.dst().port();

    if !client.write_proxy_protocol_header {
        return client.send_local(flow_dst_port, &data[data_offset..]).await;
    }

    let (header, write_offset) = match flow {
        UdpFlow::V4 { src, dst } => (
            ProxyProtocolHeader::AfInet {
                client_ip: *src.ip(),
                proxy_ip: *dst.ip(),
                client_port: src.port(),
                proxy_port: dst.port()
            },
            data_offset
                .checked_sub(UDP_PROXY_PROTOCOL_LEN_V4)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "data offset not large enough for proxy header"))?
        ),
        UdpFlow::V6 { src, dst, .. } => (
            ProxyProtocolHeader::AfInet6 {
                client_ip: src.0,
                proxy_ip: dst.0,
                client_port: src.1,
                proxy_port: dst.1,
            },
            data_offset
                .checked_sub(UDP_PROXY_PROTOCOL_LEN_V6)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "data offset not large enough for proxy header"))?
        ),
    };

    let mut writer = &mut data[write_offset..];
    header.write_v2_udp(&mut writer)?;

    assert_eq!(
        writer.as_mut_ptr() as usize,
        data.as_mut_ptr() as usize + data_offset,
        "wrote unexpected byte count with proxy protocol header"
    );

    client.send_local(flow_dst_port, &data[write_offset..]).await
}
