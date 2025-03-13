use std::{collections::{btree_map, hash_map, BTreeMap, HashMap}, future::Future, net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4}, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex}, time::{Duration, Instant}};

use playit_agent_proto::control_messages::UdpChannelDetails;
use playit_api_client::api::{PortType, ProxyProtocol};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tracing::Instrument;
use uuid::Uuid;

use crate::{agent_control::{udp_channel::{UdpChannel, UdpTunnelRx}, udp_proto::UdpFlow, DualStackUdpSocket, PacketIO}, network::{address_lookup::{AddressLookup, HostOrigin}, proxy_protocol::ProxyProtocolHeader, udp::receive_task::UdpReceiverTask}, utils::{error_helper::MaxErrorInterval, id_slab::IdSlab, now_sec}};

use super::{packets::Packets, receive_task::SocketPacket};

pub struct UdpClients<I: UdpTunnelProvider> {
    packets: Packets,
    provider: I,
    sockets: IdSlab<Socket<I::PacketIO>>,
    tunnel_socket_id: u64,
    rx_packets: Receiver<SocketPacket>,
    rx_packets_sender: Sender<SocketPacket>,
    udp_channel: UdpChannel,
    errors: ErrorLogs,
    flow_to_socket_id: BTreeMap<UdpFlow, u64>,
    udp_details: UdpDetailsSenderInner,
    last_clear_old: Instant,
}

pub struct UdpDetailsSender {
    inner: UdpDetailsSenderInner,
}

#[derive(Clone)]
struct UdpDetailsSenderInner {
    value: Arc<Mutex<Option<UdpChannelDetails>>>,
}

impl UdpDetailsSender {
    pub fn send(&mut self, value: UdpChannelDetails) {
        let mut lock = self.inner.value.lock().unwrap();
        lock.replace(value);
    }
}

impl UdpDetailsSenderInner {
    fn take(&mut self) -> Option<UdpChannelDetails> {
        let mut lock = self.value.try_lock().ok()?;
        lock.take()
    }
}

pub trait UdpTunnelProvider: AddressLookup {
    type PacketIO: PacketIO;

    fn alloc_socket(&self) -> impl Future<Output = std::io::Result<Self::PacketIO>>;
}

pub struct DualSocketTunnelProvider<L: AddressLookup> {
    lookup: L,
}

impl<L: AddressLookup> DualSocketTunnelProvider<L> {
    pub fn new(lookup: L) -> Self {
        DualSocketTunnelProvider { lookup }
    }
}

impl<L: AddressLookup> AddressLookup for DualSocketTunnelProvider<L> {
    type Value = L::Value;

    fn lookup(&self, ip: std::net::IpAddr, port: u16, proto: PortType) -> Option<crate::network::address_lookup::AddressValue<Self::Value>> {
        self.lookup.lookup(ip, port, proto)
    }
}

impl<L: AddressLookup> UdpTunnelProvider for DualSocketTunnelProvider<L> {
    type PacketIO = DualStackUdpSocket;

    async fn alloc_socket(&self) -> std::io::Result<Self::PacketIO> {
        DualStackUdpSocket::new().await
    }
}

struct ErrorLogs {
    channel: MaxErrorInterval,
    tunnel_missing: MaxErrorInterval,
    max_sockets: MaxErrorInterval,
    send: MaxErrorInterval,
    unexpected_origin: MaxErrorInterval,
    session_send_fail: MaxErrorInterval,
    pkt_send: MaxErrorInterval,
    out_of_packets: MaxErrorInterval,
}

struct Socket<I: PacketIO> {
    id: u64,
    packet_io: Arc<I>,
    run_receiver: Arc<AtomicBool>,
    clients: SocketClients,
    socket_type: SocketType,
    empty_at: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SocketType {
    Tunnel,
    Client,
}

impl<I: PacketIO> Drop for Socket<I> {
    fn drop(&mut self) {
        self.run_receiver.store(false, Ordering::Release);        
    }
}

impl<I: UdpTunnelProvider> UdpClients<I> where I::Value: Into<HostOrigin> {
    pub fn new(provider: I, tunnel_socket: Arc<I::PacketIO>, packet_count: usize) -> Self {
        assert!(2 < packet_count);

        let packets = Packets::new(packet_count);
        let (rx_packets_sender, rx_packets) = channel(packets.packet_count() / 2);

        let mut sockets = IdSlab::with_capacity(1024);
        let entry = sockets.vacant_entry().expect("first alloc of slab failed");
        let tunnel_socket_id = entry.id();

        let tunnel_socket = Socket {
            id: tunnel_socket_id,
            packet_io: tunnel_socket,
            run_receiver: Arc::new(AtomicBool::new(true)),
            clients: SocketClients::default(),
            socket_type: SocketType::Tunnel,
            empty_at: Instant::now(),
        };

        tokio::spawn(UdpReceiverTask {
            id: tunnel_socket_id,
            rx: tunnel_socket.packet_io.clone(),
            run: tunnel_socket.run_receiver.clone(),
            packets: packets.clone(),
            tx: rx_packets_sender.clone(),
            rx_offset: 0,
        }.start());

        entry.insert(tunnel_socket);

        UdpClients {
            packets,
            provider,
            sockets,
            tunnel_socket_id,
            rx_packets,
            rx_packets_sender,
            udp_channel: UdpChannel::new(),
            errors: ErrorLogs {
                channel: MaxErrorInterval::new(Duration::from_secs(2)),
                tunnel_missing: MaxErrorInterval::new(Duration::from_secs(2)),
                max_sockets: MaxErrorInterval::new(Duration::from_secs(2)),
                send: MaxErrorInterval::new(Duration::from_secs(2)),
                unexpected_origin: MaxErrorInterval::new(Duration::from_secs(2)),
                session_send_fail: MaxErrorInterval::new(Duration::from_secs(2)),
                pkt_send: MaxErrorInterval::new(Duration::from_secs(2)),
                out_of_packets: MaxErrorInterval::new(Duration::from_secs(2)),
            },
            flow_to_socket_id: Default::default(),
            udp_details: UdpDetailsSenderInner {
                value: Arc::new(Mutex::new(None)),
            },
            last_clear_old: Instant::now(),
        }
    }

    pub fn udp_channel(&self) -> UdpChannel {
        self.udp_channel.clone()
    }

    pub fn udp_details_sender(&self) -> UdpDetailsSender {
        UdpDetailsSender {
            inner: self.udp_details.clone(),
        }
    }

    fn clear_old(&mut self) {
        let mut sockets_to_remove = Vec::<u64>::new();
        let mut flows_to_remove = Vec::<UdpFlow>::new();

        for socket in self.sockets.iter_mut() {
            if socket.socket_type == SocketType::Tunnel {
                continue;
            }

            if socket.clients.clients.len() == 0 {
                if Duration::from_secs(60) < socket.empty_at.elapsed() {
                    tracing::info!(socket_id = socket.id, "removing empty socket with no recent activity");
                    sockets_to_remove.push(socket.id);
                }
                continue;
            }

            socket.clients.clients.retain(|client| {
                let keep = 'keep: {
                    let since_tunnel_activity = client.last_tunnel_activity.elapsed();

                    /* no data from host and 15s since tunnel data */
                    if client.last_host_activity.is_none() {
                        break 'keep since_tunnel_activity < Duration::from_secs(15);
                    }

                    let since_host_activity = client.last_host_activity.unwrap().elapsed();

                    /* most recent acitivty was within that last minute */
                    if Duration::from_secs(60) < since_host_activity.min(since_tunnel_activity) {
                        break 'keep false;
                    }

                    /* one side of the connection has no activity in last 5m */
                    if Duration::from_secs(300) < since_host_activity.max(since_tunnel_activity) {
                        break 'keep false;
                    }

                    true
                };

                if keep {
                    return true;
                }

                let (from_flow, to_flow) = match client.tunnel_flow {
                    TunnelFlow::V4Client { tunnel_ip, client_ip, client_port } => (
                        UdpFlow::V4 {
                            src: SocketAddrV4::new(client_ip, client_port),
                            dst: SocketAddrV4::new(tunnel_ip, client.resource.tunn_from_port),
                            frag: None,
                            extension: None,
                        },
                        UdpFlow::V4 {
                            src: SocketAddrV4::new(client_ip, client_port),
                            dst: SocketAddrV4::new(tunnel_ip, client.resource.tunn_to_port),
                            frag: None,
                            extension: None,
                        },
                    ),
                    TunnelFlow::V6Client { tunnel_ip, client_ip, client_port } => (
                        UdpFlow::V6 {
                            src: (client_ip, client_port),
                            dst: (tunnel_ip, client.resource.tunn_from_port),
                            extension: None,
                        },
                        UdpFlow::V6 {
                            src: (client_ip, client_port),
                            dst: (tunnel_ip, client.resource.tunn_to_port),
                            extension: None,
                        }
                    ),
                };

                let flow_rm_count = {
                    let count = flows_to_remove.len();
                    flows_to_remove.extend(self.flow_to_socket_id.range(from_flow..to_flow).map(|pair| pair.0));
                    flows_to_remove.len() - count
                };

                assert!(flow_rm_count != 0, "client should have at least 1 flow");

                tracing::info!(socket_id = socket.id, ?client, flow_rm_count, "removing old client from socket");
                false
            });

            if socket.clients.clients.len() == 0 {
                socket.empty_at = Instant::now();
            }
        }

        for flow in flows_to_remove {
            self.flow_to_socket_id.remove(&flow).expect("missing flow queued for remove");
        }

        for socket_id in sockets_to_remove {
            let socket = self.sockets.remove(socket_id).unwrap();
            assert_eq!(socket.clients.clients.len(), 0);
            assert_eq!(socket.socket_type, SocketType::Client);
        }
    }

    pub async fn recv_next(&mut self, timeout: Duration) {
        /* clear old connections */
        if Duration::from_secs(10) < self.last_clear_old.elapsed() {
            self.last_clear_old = Instant::now();
            
            let _span = tracing::info_span!("clear_old").entered();
            self.clear_old();
        }

        /* send UDP session details */
        {
            if let Some(udp_details) = self.udp_details.take() {
                tracing::info!(tunnel_addr = %udp_details.tunnel_addr, "updating udp details");

                let socket = self.sockets.get(self.tunnel_socket_id)
                    .expect("missing tunnel socket");

                if let Err(error) = self.udp_channel.send_token(&udp_details, &socket.packet_io).await {
                    if self.errors.session_send_fail.check() {
                        tracing::error!(?error, "failed to send udp session details");
                    }
                }
            }
            else if self.udp_channel.check_resend(now_sec()) {
                tracing::info!("resend udp session");

                let socket = self.sockets.get(self.tunnel_socket_id)
                    .expect("missing tunnel socket");

                if let Err(error) = self.udp_channel.resend_token(&socket.packet_io).await {
                    if self.errors.session_send_fail.check() {
                        tracing::error!(?error, "failed to send udp session details");
                    }
                }
            }
        }

        let mut packet = match tokio::time::timeout(timeout, self.rx_packets.recv()).await {
            Ok(item_opt) => item_opt.expect("rx packets queue closed"),
            Err(_) => return,
        };

        if packet.socket_id == self.tunnel_socket_id {
            return self.handle_tunnel_packet(packet).await;
        }

        let Some(socket) = self.sockets.get_mut(packet.socket_id) else {
            return;
        };

        assert_eq!(socket.socket_type, SocketType::Client);

        let packet_data_len = packet.packet.len();
        let packet_data = &mut packet.packet.full_slice_mut()[packet.data_offset..];

        let Some(client) = socket.clients.clients
            .iter_mut()
            .find(|client| client.resource.contains_addr(&packet.address))
        else {
            if self.errors.unexpected_origin.check() {
                tracing::error!(source = %packet.address, "unexpected tunnel origin");
            }
            return;
        };

        client.last_host_activity = Some(Instant::now());

        let client_flow = {
            let port_offset = packet.address.port() - client.resource.host_origin.port();
            let tunnel_port = client.resource.tunn_from_port + port_offset;

            match client.tunnel_flow {
                TunnelFlow::V4Client { tunnel_ip, client_ip, client_port } => UdpFlow::V4 {
                    src: SocketAddrV4::new(client_ip, client_port),
                    dst: SocketAddrV4::new(tunnel_ip, tunnel_port),
                    frag: None,
                    extension: None,
                },
                TunnelFlow::V6Client { tunnel_ip, client_ip, client_port } => UdpFlow::V6 {
                    src: (client_ip, client_port),
                    dst: (tunnel_ip, tunnel_port),
                    extension: None,
                },
            }
        };

        assert_eq!(self.flow_to_socket_id.get(&client_flow).cloned(), Some(packet.socket_id));

        let tunnel_socket = self.sockets.get(self.tunnel_socket_id).expect("missing tunnel socket");
        assert_eq!(tunnel_socket.socket_type, SocketType::Tunnel);

        let res = self.udp_channel.send_host_pkt(
            packet_data,
            packet_data_len,
            client_flow.flip(),
            &tunnel_socket.packet_io,
        ).await;

        if let Err(error) = res {
            if self.errors.pkt_send.check() {
                tracing::error!(?error, "failed to send packet to client");
            }
        }
    }

    async fn handle_tunnel_packet(&mut self, mut packet: SocketPacket) {
        let parse_res = self.udp_channel.parse_packet(
            &packet.packet.full_slice()[packet.data_offset..],
            packet.packet.len() - packet.data_offset,
            packet.address
        ).await;

        let rx = match parse_res {
            Ok(res) => res,
            Err(error) => {
                if self.errors.channel.check() {
                    tracing::error!(?error, "got error parsing udp tunnel packet");
                }
                return;
            },
        };

        match rx {
            UdpTunnelRx::ReceivedPacket { bytes, flow } => {
                self.forward_packet_to_origin(flow, packet.packet.full_slice_mut(), packet.data_offset, bytes).await;
            }
            UdpTunnelRx::ConfirmedConnection => {
                tracing::info!("UDP session confirmed");
            }
            UdpTunnelRx::UpdatedConnection => {
                tracing::info!("UDP session updated");
            }
        }
    }

    async fn forward_packet_to_origin(&mut self, flow_path: UdpFlow, buffer: &mut [u8], data_start: usize, data_len: usize) {
        let mut now = Instant::now();

        let socket_id = match self.flow_to_socket_id.entry(flow_path) {
            btree_map::Entry::Occupied(o) => *o.into_mut(),
            btree_map::Entry::Vacant(v) => {
                let path = v.key();
                let Some(found) = self.provider.lookup(path.dst().ip(), path.dst().port(), PortType::Udp) else {
                    if self.errors.tunnel_missing.check() {
                        tracing::error!(?path, "could not find tunnel for new flow");
                    }
                    return;
                };

                let host_origin: HostOrigin = found.value.into();
                assert!(found.from_port <= flow_path.dst().port());
                assert!(flow_path.dst().port() < found.to_port);

                let uses_proxy_protocol = host_origin.proxy_protocol == Some(ProxyProtocol::ProxyProtocolV2);
                tracing::info!(uses_proxy_protocol, tunnel_id = %host_origin.tunnel_id, ?flow_path, "new UDP client");

                let socket_client = SocketClient {
                    tunnel_id: host_origin.tunnel_id,
                    resource: HostResource {
                        host_origin: host_origin.host_addr,
                        tunn_from_port: found.from_port,
                        tunn_to_port: found.to_port,
                    },
                    tunnel_flow: match flow_path {
                        UdpFlow::V4 { src, dst, .. } => TunnelFlow::V4Client { tunnel_ip: *dst.ip(), client_ip: *src.ip(), client_port: src.port() },
                        UdpFlow::V6 { src, dst, .. } => TunnelFlow::V6Client { tunnel_ip: dst.0, client_ip: src.0, client_port: src.1 },
                    },
                    last_tunnel_activity: now,
                    last_host_activity: None,
                    uses_proxy_protocol,
                    last_proxy_packet: None,
                };

                let socket = self.sockets.iter_mut().find(|socket| {
                    socket.clients.can_add(&socket_client.resource)
                });

                let socket_id = match socket {
                    Some(socket) => {
                        socket.clients.clients.push(socket_client);
                        socket.id
                    }
                    None => {
                        let Some(socket_entry) = self.sockets.vacant_entry() else {
                            if self.errors.max_sockets.check() {
                                tracing::error!("no viable socket found for new client, reached socket limit");
                            }
                            return;
                        };

                        let new_io = match self.provider.alloc_socket().await {
                            Ok(v) => v,
                            Err(error) => {
                                tracing::error!(?error, "failed to setup new socket");
                                return;
                            }
                        };

                        let socket = Socket {
                            id: socket_entry.id(),
                            packet_io: Arc::new(new_io),
                            run_receiver: Arc::new(AtomicBool::new(true)),
                            clients: SocketClients { clients: vec![socket_client] },
                            socket_type: SocketType::Client,
                            empty_at: Instant::now(),
                        };

                        tokio::spawn(UdpReceiverTask {
                            id: socket.id,
                            rx: socket.packet_io.clone(),
                            run: socket.run_receiver.clone(),
                            packets: self.packets.clone(),
                            tx: self.rx_packets_sender.clone(),
                            rx_offset: 0,
                        }.start());

                        socket_entry.insert(socket)
                    }
                };

                now = Instant::now();
                *v.insert(socket_id)
            }
        };

        let socket = self.sockets.get_mut(socket_id)
            .expect("could not load socket from id");

        let client = socket.clients.get_client_mut(&flow_path).expect("could not find client");
        client.last_tunnel_activity = now;

        let target_addr = SocketAddr::new(
            client.resource.host_origin.ip(),
            client.resource.host_origin.port() + flow_path.dst().port() - client.resource.tunn_from_port
        );

        'send_proxy_packet: {
            if !client.uses_proxy_protocol {
                break 'send_proxy_packet;
            }

            /* have recent packets from host, they must be okay with client so need to resend proxy protocol */
            if let Some(host_ts) = &client.last_host_activity {
                if host_ts.elapsed() < Duration::from_secs(15) {
                    break 'send_proxy_packet;
                }
            }

            /* don't send proxy protocol packet more than once ever 2s */
            if let Some(send_ts) = &client.last_proxy_packet {
                if send_ts.elapsed() < Duration::from_secs(2) {
                    break 'send_proxy_packet;
                }
            }

            /* Send proxy protocol header to establish true origin IP */
            let header = ProxyProtocolHeader::from_udp_flow(&flow_path);
            if let Some(mut packet) = self.packets.allocate() {
                let len = {
                    let mut writer = packet.full_slice_mut();
                    let og_len = writer.len();
                    header.write_v2_udp(&mut writer).expect("should be plenty space for proxy pass header in packet");
                    og_len - writer.len()
                };

                packet.set_len(len).expect("len should be within bounds");

                /* send proxy protocol packet */
                {
                    if let Err(error) = socket.packet_io.send_to(packet.as_ref(), target_addr).await {
                        if self.errors.send.check() {
                            tracing::error!(?error, "failed to send PROXY PROTOCOL V2 packet");
                        }
                    }
                }

                tracing::info!(target = %target_addr, "send proxy protocol v2 header");
                client.last_proxy_packet = Some(now);
            } else {
                if self.errors.out_of_packets.check() {
                    tracing::error!("out of free packets, failed to send PROXY PROTOCOL V2 header");
                }
            }
        }

        if let Err(error) = socket.packet_io.send_to(&buffer[data_start..(data_start + data_len)], target_addr).await {
            if self.errors.send.check() {
                tracing::error!(?error, "failed to send packet");
            }
        }
    }
}

#[derive(Default)]
pub struct SocketClients {
    clients: Vec<SocketClient>,
}

impl SocketClients {
    fn can_add(&self, resource: &HostResource) -> bool {
        self.clients.iter()
            .find(|c| !c.resource.is_overlapping(resource))
            .is_some()
    }
    
    fn get_client_mut(&mut self, flow: &UdpFlow) -> Option<&mut SocketClient> {
        self.clients.iter_mut().find(|client| {
            let (same_tunnel_ip, dst_port) = match (&client.tunnel_flow, flow) {
                (TunnelFlow::V4Client { tunnel_ip, .. }, UdpFlow::V4 { dst, .. }) => (tunnel_ip.eq(dst.ip()), dst.port()),
                (TunnelFlow::V6Client { tunnel_ip, .. }, UdpFlow::V6 { dst, .. }) => (tunnel_ip.eq(&dst.0), dst.1),
                _ => (false, 0),
            };

            if !same_tunnel_ip {
                return false;
            }

            client.resource.tunn_from_port <= dst_port && dst_port < client.resource.tunn_to_port
        })
    }
}


#[derive(Debug)]
pub struct SocketClient {
    pub tunnel_id: Uuid,
    pub resource: HostResource,
    pub tunnel_flow: TunnelFlow,

    pub last_tunnel_activity: Instant,
    pub last_host_activity: Option<Instant>,

    pub uses_proxy_protocol: bool,
    pub last_proxy_packet: Option<Instant>,
}

#[derive(Debug)]
pub struct HostResource {
    pub host_origin: SocketAddr,
    pub tunn_from_port: u16,
    pub tunn_to_port: u16,
}

#[derive(Debug)]
pub enum TunnelFlow {
    V4Client {
        tunnel_ip: Ipv4Addr,
        client_ip: Ipv4Addr,
        client_port: u16,
    },
    V6Client {
        tunnel_ip: Ipv6Addr,
        client_ip: Ipv6Addr,
        client_port: u16,
    },
}

impl HostResource {
    fn contains_addr(&self, addr: &SocketAddr) -> bool {
        if self.host_origin.ip() != addr.ip() {
            return false;
        }

        let port_count = self.tunn_to_port - self.tunn_from_port;
        let port = self.host_origin.port();
        let addr_port = addr.port();

        port <= addr_port && addr_port < (port + port_count)
    }

    fn is_overlapping(&self, other: &Self) -> bool {
        if !self.host_origin.ip().eq(&other.host_origin.ip()) {
            return false;
        }

        let a_start = self.host_origin.port();
        let a_end = a_start + (self.tunn_to_port - self.tunn_from_port);

        let b_start = other.host_origin.port();
        let b_end = b_start + (other.tunn_to_port - other.tunn_from_port);

        a_start.max(b_start) <= a_end.min(b_end)
    }
}

