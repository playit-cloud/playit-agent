use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use byteorder::{BigEndian, ByteOrder};
use tokio::net::UdpSocket;

use tokio::sync::RwLock;
use playit_agent_common::agent_config::get_match_ip;

use playit_agent_common::{Proto, SetupUdpChannelDetails};
use playit_agent_common::udp::{RedirectFlowFooter, UDP_CHANNEL_ESTABLISH_ID};

use crate::agent_state::{AgentState, KeepFreshTimes};
use crate::lan_address::LanAddress;
use crate::now_milli;

pub struct UdpTunnel {
    udp: Arc<UdpSocket>,
    udp_clients: RwLock<HashMap<Flow, Arc<UdpClient>>>,
    keep_alive: KeepFreshTimes,
    state: Arc<AgentState>,
}

impl UdpTunnel {
    pub async fn new(state: Arc<AgentState>, tunnel_addr: SocketAddr) -> std::io::Result<Self> {
        let bind_addr: SocketAddr = match tunnel_addr {
            SocketAddr::V4(_) => SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into(),
            SocketAddr::V6(_) => SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0).into(),
        };

        let udp = Arc::new(UdpSocket::bind(bind_addr).await?);

        Ok(UdpTunnel {
            udp,
            udp_clients: Default::default(),
            keep_alive: Default::default(),
            state
        })
    }

    pub fn is_active(&self) -> bool {
        self.keep_alive.has_ack()
    }

    pub async fn client_count(&self) -> usize {
        let clients_lock = self.udp_clients.read().await;
        clients_lock.len()
    }

    pub async fn get_udp_clients(&self, tunnel_ip: IpAddr, tunnel_from_port: u16, tunnel_to_port: u16) -> Vec<Arc<UdpClient>> {
        let clients_lock = self.udp_clients.read().await;
        let search_ip = get_match_ip(tunnel_ip);

        let mut res = Vec::new();
        for (flow, client) in &*clients_lock {

            let matches = get_match_ip(flow.tunnel_addr.ip()) == search_ip
                && tunnel_from_port <= flow.tunnel_addr.port()
                && flow.tunnel_addr.port() < tunnel_to_port;

            if matches {
                res.push(client.clone());
            }
        }

        res
    }

    pub async fn run_keepalive(self: Arc<Self>) {
        loop {
            if let Err(wait) = self.keep_alive.try_send(3_000, 25_000) {
                tokio::time::sleep(Duration::from_millis(wait)).await;
                continue;
            }

            let details = self.state.udp_channel_details.read().await;
            if let Some(details) = &*details {
                if let Err(error) = self.udp.send_to(&details.token, &details.tunnel_addr).await {
                    tracing::error!(?error, "failed to send UDP keep alive");
                }
            }
        }
    }

    pub async fn handle_packets(self: Arc<Self>) {
        let mut buffer = vec![0u8; 2048];

        loop {
            tokio::task::yield_now().await;

            let (bytes, source) = match self.udp.recv_from(&mut buffer).await {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "error reading from UDP socket");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            if bytes < 8 {
                tracing::warn!(bytes, "got invalid tiny UDP channel message");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }

            /* get trailing ID */
            let id = BigEndian::read_u64(&buffer[bytes - 8..bytes]);
            if id == UDP_CHANNEL_ESTABLISH_ID {
                tracing::info!("got UDP establish response");
                self.keep_alive.ack();
                continue;
            }

            let footer = match RedirectFlowFooter::from_tail(&buffer[..bytes]) {
                Some(v) => v,
                None => {
                    tracing::error!(id, bytes, "got channel message with unknown id");
                    continue;
                }
            };

            let flow = Flow {
                tunnel_addr: footer.dst(),
                client_addr: footer.src(),
            };

            let payload = &buffer[..bytes - footer.len()];

            /* route to existing client if exists */
            {
                let clients = self.udp_clients.read().await;

                if let Some(client) = clients.get(&flow) {
                    if let Err(error) = client.send_payload(payload, source).await {
                        tracing::error!(?error, "failed to send payload to host");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }

                    continue;
                }
            }

            /* new client, verify source */
            {
                let channel_details = self.state.udp_channel_details.read().await;
                match &*channel_details {
                    Some(details) => {
                        if details.tunnel_addr != source {
                            continue;
                        }
                    }
                    None => continue,
                }
            }


            /* determine tunnel client should be directed to */
            let (bind_addr, local_address) = {
                let agent_config = self.state.agent_config.read().await;
                match agent_config.find_local_addr(flow.tunnel_addr, Proto::Udp) {
                    Some(v) => v,
                    None => {
                        tracing::warn!("could not find tunnel for client");
                        continue;
                    }
                }
            };

            /* setup new client */
            {
                let host_udp_res = match (bind_addr, local_address) {
                    (Some(IpAddr::V4(bind_addr)), SocketAddr::V4(_)) => {
                        UdpSocket::bind(SocketAddrV4::new(bind_addr, 0)).await
                    }
                    (Some(IpAddr::V6(bind_addr)), SocketAddr::V6(_)) => {
                        UdpSocket::bind(SocketAddrV6::new(bind_addr, 0, 0, 0)).await
                    }
                    (None, local_address) => {
                        LanAddress::udp_socket(true, flow.client_addr, local_address).await
                    }
                    (Some(bind), local) => {
                        tracing::error!(?bind, ?local, "cannot bind using different protocol than local address");
                        continue;
                    }
                };

                let host_udp = match host_udp_res {
                    Ok(v) => v,
                    Err(error) => {
                        tracing::error!(?error, "failed to setup UDP socket for client");
                        continue;
                    }
                };

                let mut clients = self.udp_clients.write().await;

                let udp_client = Arc::new(UdpClient {
                    host_udp,
                    tunnel_udp: self.udp.clone(),
                    udp_tunnel: self.clone(),
                    flow_key: flow.clone(),
                    last_usage: AtomicU64::new(now_milli()),
                    active: AtomicBool::new(true),
                    to_tunnel_packets: Default::default(),
                    to_tunnel_bytes: Default::default(),
                    to_host_packets: Default::default(),
                    to_host_bytes: Default::default(),
                    to_tunnel_flow: footer.flip(),
                    udp_tunnel_addr: source,
                    host_forward_addr: local_address,
                });

                /* start task to forward packets from host */
                tokio::spawn(HostToTunnelForwarder(udp_client.clone()).run());

                if let Err(error) = udp_client.send_payload(payload, source).await {
                    tracing::error!(?error, "failed to send initial UDP packet to client");
                }

                clients.insert(flow, udp_client);
            }
        }
    }
}

#[derive(Hash, PartialEq, Eq, Clone)]
pub struct Flow {
    pub tunnel_addr: SocketAddr,
    pub client_addr: SocketAddr,
}

pub struct UdpClient {
    pub host_udp: UdpSocket,
    pub tunnel_udp: Arc<UdpSocket>,
    pub udp_tunnel: Arc<UdpTunnel>,

    pub flow_key: Flow,

    pub last_usage: AtomicU64,
    pub active: AtomicBool,

    pub to_tunnel_packets: AtomicU64,
    pub to_tunnel_bytes: AtomicU64,

    pub to_host_packets: AtomicU64,
    pub to_host_bytes: AtomicU64,

    pub to_tunnel_flow: RedirectFlowFooter,

    pub udp_tunnel_addr: SocketAddr,
    pub host_forward_addr: SocketAddr,
}

impl UdpClient {
    async fn send_payload(&self, payload: &[u8], source: SocketAddr) -> std::io::Result<()> {
        if self.udp_tunnel_addr != source {
            return Ok(());
        }

        self.host_udp.send_to(payload, self.host_forward_addr).await?;
        self.last_usage.store(now_milli(), Ordering::Release);
        Ok(())
    }
}

struct HostToTunnelForwarder(pub Arc<UdpClient>);

impl HostToTunnelForwarder {
    pub async fn run(self) {
        let mut buffer = vec![0u8; 2048];

        loop {
            tokio::task::yield_now().await;

            let (bytes, source) = match tokio::time::timeout(Duration::from_secs(60), self.0.host_udp.recv_from(&mut buffer)).await {
                Ok(Ok(v)) => v,
                Ok(Err(error)) => {
                    tracing::error!(?error, "failed to receive data from host socket");
                    continue;
                }
                Err(_) => {
                    let last_usage = self.0.last_usage.load(Ordering::Acquire);
                    if now_milli() - last_usage > 60_000 {
                        break;
                    }
                    continue;
                }
            };

            self.0.last_usage.store(now_milli(), Ordering::Relaxed);
            self.0.to_tunnel_packets.fetch_add(1, Ordering::Relaxed);
            self.0.to_tunnel_bytes.fetch_add(bytes as _, Ordering::Relaxed);

            if source != self.0.host_forward_addr {
                continue;
            }

            let footer_len = self.0.to_tunnel_flow.len();
            if buffer.len() < bytes + footer_len {
                continue;
            }

            let updated_len = bytes + footer_len;
            let success = self.0.to_tunnel_flow.write_to(&mut buffer[bytes..updated_len]);
            assert!(success);

            if let Err(error) = self.0.tunnel_udp.send_to(&buffer[..updated_len], self.0.udp_tunnel_addr).await {
                tracing::error!(?error, "failed to forward packet to tunnel");
            }
        }

        let removed = {
            let mut clients = self.0.udp_tunnel.udp_clients.write().await;
            clients.remove(&self.0.flow_key)
        };

        match removed {
            None => {
                tracing::error!("could not find UDP client when removing");
            }
            Some(v) if !Arc::ptr_eq(&v, &self.0) => {
                tracing::error!("removing different UDP client when closing");
            }
            _ => {}
        }

        self.0.active.store(false, Ordering::SeqCst);
    }
}