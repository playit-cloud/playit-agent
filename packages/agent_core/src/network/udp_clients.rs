use std::collections::{HashMap, hash_map::Entry};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use byteorder::{BigEndian, ByteOrder};
use hyper::Client;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use playit_agent_common::agent_config::get_match_ip;
use playit_agent_common::Proto;
use playit_agent_common::udp::{RedirectFlowFooter, UDP_CHANNEL_ESTABLISH_ID};
use playit_agent_proto::PortProto;

use crate::agent_state::{AgentState, KeepFreshTimes};
use crate::lan_address::LanAddress;
use crate::network::address_lookup::{AddressLookup, MatchAddress};
use crate::now_milli;
use crate::tunnel::udp_proto::UdpFlow;
use crate::tunnel::udp_tunnel::UdpTunnel;

pub struct UdpClients<L: AddressLookup> {
    tunnel: UdpTunnel,
    lookup: L,
    udp_clients: RwLock<HashMap<ClientKey, Arc<UdpClient>>>,
}

#[derive(Eq, PartialEq, Hash, Debug)]
struct ClientKey {
    client_addr: SocketAddr,
    tunnel_addr: SocketAddr,
}

impl<L: AddressLookup> UdpClients<L> {
    pub fn new(tunnel: UdpTunnel, lookup: L) -> Self {
        UdpClients {
            tunnel,
            lookup,
            udp_clients: Default::default(),
        }
    }

    pub async fn client_count(&self) -> usize {
        let clients_lock = self.udp_clients.read().await;
        clients_lock.len()
    }

    pub async fn forward_packet(&self, flow: &UdpFlow, data: &[u8]) -> std::io::Result<usize> {
        let flow_dst = flow.dst();
        let match_addr = match self.lookup.tunnel_match_address(flow_dst) {
            Some(v) => v,
            None => return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "could not find tunnel")),
        };

        /* normalize port */
        let key = ClientKey {
            client_addr: flow.src(),
            tunnel_addr: SocketAddr::new(flow.dst().ip(), match_addr.port),
        };

        {
            let clients = self.udp_clients.read().await;

            if let Some(client) = clients.get(&key) {
                return client.send_local(flow_dst.port(), data).await;
            }
        }

        {
            let mut clients = self.udp_clients.write().await;
            match clients.entry(key) {
                Entry::Occupied(o) => {
                    return o.into_mut().send_local(flow_dst.port(), data).await;
                }
                Entry::Vacant(v) => {
                    let client = Arc::new(UdpClient {
                        send_flow: match flow {
                            UdpFlow::V4 { src, dst } => UdpFlow::V4 {
                                src:
                            }
                        },
                        local_udp: ,
                        udp_tunnel: Default::default(),
                        local_start_addr: (),
                        tunnel_start_port: 0
                    });
                    v.insert(Arc::n)
                }
            }
        }

        let local_addr = match self.lookup.local_address(match_addr, PortProto::Udp) {
            Some(v) => v,
            None => return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "could not find tunnel")),
        };


        todo!()
    }
}

pub struct UdpClient {
    pub send_flow: UdpFlow,
    pub local_udp: UdpSocket,
    pub udp_tunnel: UdpTunnel,
    pub local_start_addr: SocketAddr,
    pub tunnel_start_port: u16,
}

impl UdpClient {
    pub async fn send_local(&self, dst_port: u16, data: &[u8]) -> std::io::Result<usize> {
        let port_offset = dst_port - self.tunnel_start_port;

        let target_addr = if port_offset == 0 {
            self.local_start_addr
        } else {
            SocketAddr::new(self.local_start_addr.ip(), self.local_start_addr.port() + port_offset)
        };

        self.local_udp.send_to(data, target_addr).await
    }
}

// impl UdpClient {
//     async fn send_payload(&self, payload: &[u8], source: SocketAddr) -> std::io::Result<()> {
//         if self.udp_tunnel_addr != source {
//             return Ok(());
//         }
//
//         self.local_udp.send_to(payload, self.host_forward_addr).await?;
//         self.last_usage.store(now_milli(), Ordering::Release);
//         Ok(())
//     }
// }
//
// struct HostToTunnelForwarder(pub Arc<UdpClient>);
//
// impl HostToTunnelForwarder {
//     pub async fn run(self) {
//         let mut buffer = vec![0u8; 2048];
//
//         loop {
//             tokio::task::yield_now().await;
//
//             let (bytes, source) = match tokio::time::timeout(Duration::from_secs(60), self.0.local_udp.recv_from(&mut buffer)).await {
//                 Ok(Ok(v)) => v,
//                 Ok(Err(error)) => {
//                     tracing::error!(?error, "failed to receive data from host socket");
//                     continue;
//                 }
//                 Err(_) => {
//                     let last_usage = self.0.last_usage.load(Ordering::Acquire);
//                     if now_milli() - last_usage > 60_000 {
//                         break;
//                     }
//                     continue;
//                 }
//             };
//
//             self.0.last_usage.store(now_milli(), Ordering::Relaxed);
//             self.0.to_tunnel_packets.fetch_add(1, Ordering::Relaxed);
//             self.0.to_tunnel_bytes.fetch_add(bytes as _, Ordering::Relaxed);
//
//             if source != self.0.host_forward_addr {
//                 continue;
//             }
//
//             let footer_len = self.0.to_tunnel_flow.len();
//             if buffer.len() < bytes + footer_len {
//                 continue;
//             }
//
//             let updated_len = bytes + footer_len;
//             let success = self.0.to_tunnel_flow.write_to(&mut buffer[bytes..updated_len]);
//             assert!(success);
//
//             if let Err(error) = self.0.tunnel_udp.send_to(&buffer[..updated_len], self.0.udp_tunnel_addr).await {
//                 tracing::error!(?error, "failed to forward packet to tunnel");
//             }
//         }
//
//         let removed = {
//             let mut clients = self.0.udp_tunnel.udp_clients.write().await;
//             clients.remove(&self.0.flow_key)
//         };
//
//         match removed {
//             None => {
//                 tracing::error!("could not find UDP client when removing");
//             }
//             Some(v) if !Arc::ptr_eq(&v, &self.0) => {
//                 tracing::error!("removing different UDP client when closing");
//             }
//             _ => {}
//         }
//
//         self.0.active.store(false, Ordering::SeqCst);
//     }
// }