use std::collections::{hash_map::Entry, HashMap};
use std::future::Future;
use std::net::{IpAddr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use slab::Slab;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use agent_common::{Proto, SetupUdpChannelDetails};
use agent_common::udp::RedirectFlowFooter;

use crate::events::{PlayitEventDetails, PlayitEvents};
use crate::lan_address::LanAddress;

#[derive(Clone)]
pub struct UdpClients {
    tunnel_udp: Arc<UdpSocket>,
    channel_details: Arc<RwLock<SetupUdpChannelDetails>>,
    client_ids: Arc<AtomicU64>,
    events: PlayitEvents,

    lookup: HashMap<(SocketAddrV4, SocketAddrV4), usize>,
    udp_client: Slab<(JoinHandle<()>, Arc<UdpClient>)>,
}

impl UdpClients {
    pub fn new(
        tunnel_udp: Arc<UdpSocket>,
        channel_details: Arc<RwLock<SetupUdpChannelDetails>>,
        client_ids: Arc<AtomicU64>,
        events: PlayitEvents,
    ) -> Self {
        UdpClients {
            tunnel_udp,
            channel_details,
            client_ids,
            events,
            lookup: Default::default(),
            udp_client: Default::default(),
        }
    }

    pub async fn forward_packet<T: Future<Output=Option<(Option<IpAddr>, SocketAddr)>>, F: FnOnce(SocketAddrV4) -> T>(
        &mut self,
        flow: RedirectFlowFooter,
        data: &[u8],
        lookup: F,
    ) {
        let client_id = match self.lookup.entry((flow.src, flow.dst)) {
            Entry::Occupied(o) => *o.into_mut(),
            Entry::Vacant(v) => {
                let (local_addr, host_addr) = match lookup(flow.dst).await {
                    Some(host_addr) => {
                        tracing::info!(?flow, ?host_addr, "found mapping for new udp client");
                        host_addr
                    }
                    None => {
                        tracing::info!(?flow, "did not find mapping for new udp client");
                        return;
                    }
                };

                let client_id = self.client_ids.fetch_add(1, Ordering::SeqCst);

                self.events.add_event(PlayitEventDetails::ClientAccepted {
                    client_id,
                    proto: Proto::Udp,
                    tunnel_addr: SocketAddr::V4(flow.dst),
                    peer_addr: SocketAddr::V4(flow.src),
                    host_addr,
                }).await;

                self.events.add_event(PlayitEventDetails::ClientConnected {
                    client_id,
                }).await;

                let host_udp_res = match local_addr {
                    Some(ip) => UdpSocket::bind(SocketAddr::new(ip, 0)).await,
                    None => LanAddress::udp_socket(true, SocketAddr::V4(flow.src), host_addr).await
                };

                let host_udp = match host_udp_res {
                    Ok(v) => v,
                    Err(error) => {
                        tracing::error!(?error, "failed to create UDP socket for new client");
                        return;
                    }
                };

                let client = Arc::new(UdpClient {
                    from_client_flow: flow,
                    to_tunnel_flow: flow.flip(),
                    host_udp,
                    host_addr,
                });

                let forwarder = UdpClientForwarder {
                    client: client.clone(),
                    channel_details: self.channel_details.clone(),
                    tunnel_udp: self.tunnel_udp.clone(),
                };

                let handle = tokio::spawn(forwarder.run());
                *v.insert(self.udp_client.insert((handle, client)))
            }
        };

        let (_, client) = self.udp_client.get_mut(client_id).unwrap();
        if let Err(error) = client.host_udp.send_to(data, client.host_addr).await {
            tracing::error!(?error, "failed to forward packet to host");
        }
    }
}

impl Drop for UdpClients {
    fn drop(&mut self) {
        for (_, (handle, _)) in &mut self.udp_client {
            handle.abort();
        }
    }
}

#[allow(dead_code)]
struct UdpClient {
    from_client_flow: RedirectFlowFooter,
    to_tunnel_flow: RedirectFlowFooter,
    host_udp: UdpSocket,
    host_addr: SocketAddr,
}

struct UdpClientForwarder {
    pub client: Arc<UdpClient>,
    pub channel_details: Arc<RwLock<SetupUdpChannelDetails>>,
    pub tunnel_udp: Arc<UdpSocket>,
}

impl UdpClientForwarder {
    pub async fn run(self) {
        let mut buffer = vec![0u8; 2048];

        loop {
            let (bytes, from) = match self.client.host_udp.recv_from(&mut buffer).await {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to read packet from host");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            if from != self.client.host_addr {
                continue;
            }

            if buffer.len() < bytes + RedirectFlowFooter::len() {
                continue;
            }

            let updated_len = bytes + RedirectFlowFooter::len();
            let success = self
                .client
                .to_tunnel_flow
                .write_to(&mut buffer[bytes..updated_len]);
            assert!(success);

            let res = {
                let tunnel_addr = self.channel_details.read().await.tunnel_addr;
                self.tunnel_udp
                    .send_to(&buffer[..bytes + RedirectFlowFooter::len()], tunnel_addr)
                    .await
            };

            if let Err(error) = res {
                tracing::error!(?error, "failed to forward packet to tunnel");
            }
        }
    }
}
