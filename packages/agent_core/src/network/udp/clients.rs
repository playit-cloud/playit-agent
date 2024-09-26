use std::{collections::{hash_map, HashMap}, future::Future, net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4}, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex}, time::{Duration, Instant}};

use playit_agent_proto::control_messages::UdpChannelDetails;
use playit_api_client::api::{PortType, ProxyProtocol};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use uuid::Uuid;

use crate::{agent_control::{udp_channel::{UdpChannel, UdpTunnelRx}, udp_proto::UdpFlow, DualStackUdpSocket, PacketIO}, network::{address_lookup::{AddressLookup, HostOrigin}, proxy_protocol::{ProxyProtocolHeader, UDP_PROXY_PROTOCOL_LEN_V4, UDP_PROXY_PROTOCOL_LEN_V6, UDP_PROXY_PROTOCOL_MAX_LEN}, udp::receive_task::UdpReceiverTask}, utils::{error_helper::MaxErrorInterval, id_slab::IdSlab, non_overlapping::{NonOverlapping, NonOverlappingCheck}, now_sec}};

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
    conn_info: ConnectionInfo,
    udp_details: UdpDetailsSenderInner,
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
    proxy_invalid_tunnel_addr: MaxErrorInterval,
    client_flow_not_found: MaxErrorInterval,
}

#[derive(Default)]
struct ConnectionInfo {
    flows: HashMap<UdpFlow, Flow>,
}

struct Flow {
    socket_id: u64,
    target_addr: SocketAddr,
    use_proxy_protocol: bool,
    last_client_packet: Instant,
    last_host_packet: Instant,
}

struct Socket<I: PacketIO> {
    id: u64,
    packet_io: Arc<I>,
    run_receiver: Arc<AtomicBool>,
    endpoints: NonOverlapping<TunnelEndpoint>,
    socket_type: SocketType,
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
            endpoints: NonOverlapping::new(),
            socket_type: SocketType::Tunnel,
        };

        tokio::spawn(UdpReceiverTask {
            id: tunnel_socket_id,
            rx: tunnel_socket.packet_io.clone(),
            run: tunnel_socket.run_receiver.clone(),
            packets: packets.clone(),
            tx: rx_packets_sender.clone(),
            rx_offset: UDP_PROXY_PROTOCOL_MAX_LEN,
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
                proxy_invalid_tunnel_addr: MaxErrorInterval::new(Duration::from_secs(2)),
                client_flow_not_found: MaxErrorInterval::new(Duration::from_secs(2)),
            },
            conn_info: Default::default(),
            udp_details: UdpDetailsSenderInner {
                value: Arc::new(Mutex::new(None)),
            }
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

    pub async fn recv_next(&mut self, timeout: Duration) {
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

        let Some(socket) = self.sockets.get(packet.socket_id) else {
            return;
        };

        assert_ne!(socket.socket_type, SocketType::Tunnel);

        let mut packet_data_len = packet.packet.len();
        let mut packet_data = &mut packet.packet.full_slice_mut()[packet.data_offset..];

        let mut client_flow = None;

        for tunnel in socket.endpoints.iter() {
            if tunnel.host_origin.ip() != packet.address.ip() {
                continue;
            }

            let origin_from_port = tunnel.host_origin.port();
            let origin_to_port = origin_from_port + (tunnel.to_port - tunnel.from_port);
            let packet_port = packet.address.port();

            client_flow = Some({
                if packet_port < origin_from_port || origin_to_port <= packet_port {
                    continue;
                }

                let tunnel_port = tunnel.from_port + (packet_port - origin_from_port);

                match tunnel.tunnel_flow {
                    TunnelFlow::V4Client { tunnel_ip, client_ip, client_port } => UdpFlow::V4 {
                        src: SocketAddrV4::new(client_ip, client_port),
                        dst: SocketAddrV4::new(tunnel_ip, tunnel_port),
                    },
                    TunnelFlow::V6Client { tunnel_ip, client_ip, client_port } => UdpFlow::V6 {
                        src: (client_ip, client_port),
                        dst: (tunnel_ip, tunnel_port),
                        flow: 0,
                    },
                    _ => panic!("tunnel flow for DirectClient socket is for a proxy"),
                }
            });

            break;
        }

        let Some(client_flow) = client_flow else {
            if self.errors.unexpected_origin.check() {
                tracing::error!(source = %packet.address, "unexpected tunnel origin");
            }
            return;
        };

        /* validate that flow exists */
        match self.conn_info.flows.get_mut(&client_flow) {
            Some(flow) => {
                flow.last_host_packet = Instant::now();

                if flow.socket_id != socket.id {
                    if self.errors.client_flow_not_found.check() {
                        tracing::error!(?client_flow, expected_id = flow.socket_id, actual_id = socket.id, "flow expected on a different socket");
                    }
                    return;
                }

                /* parse optional proxy header */
                if flow.use_proxy_protocol {
                    'parse_proxy_header: {
                        let mut reader = &packet_data[..packet_data_len];
                        let Some(header) = ProxyProtocolHeader::parse_v2_udp(&mut reader) else {
                            break 'parse_proxy_header;
                        };
    
                        /* update packet length to trim header */
                        {
                            let header_read_len = packet_data_len - reader.len();
                            packet_data = &mut packet_data[header_read_len..];
                            packet_data_len -= header_read_len;
                        }

                        /* validate header matches flow, otherwise drop packet */
                        {
                            let parsed_flow = match header {
                                ProxyProtocolHeader::AfInet { client_ip, proxy_ip, client_port, proxy_port } => {
                                    UdpFlow::V4 {
                                        src: SocketAddrV4::new(client_ip, client_port),
                                        dst: SocketAddrV4::new(proxy_ip, proxy_port),
                                    }
                                }
                                ProxyProtocolHeader::AfInet6 { client_ip, proxy_ip, client_port, proxy_port } => {
                                    UdpFlow::V6 {
                                        src: (client_ip.into(), client_port),
                                        dst: (proxy_ip.into(), proxy_port),
                                        flow: 0,
                                    }
                                }
                            };

                            if client_flow != parsed_flow {
                                if self.errors.proxy_invalid_tunnel_addr.check() {
                                    tracing::error!(expected = ?client_flow, actual = ?parsed_flow, "proxy header is invalid");
                                }
                                return;
                            }
                        }
                    }
                }
            }
            None => {
                if self.errors.client_flow_not_found.check() {
                    tracing::error!(?client_flow, "origin trying to send to client that doesn't have flow");
                }
                return;
            }
        }

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

    async fn forward_packet_to_origin(&mut self, flow_path: UdpFlow, buffer: &mut [u8], mut data_start: usize, mut data_len: usize) {
        let mut now = Instant::now();

        let flow = match self.conn_info.flows.entry(flow_path) {
            hash_map::Entry::Occupied(o) => o.into_mut(),
            hash_map::Entry::Vacant(v) => {
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

                let target_addr = SocketAddr::new(
                    host_origin.host_addr.ip(),
                    host_origin.host_addr.port() + flow_path.dst().port() - found.from_port
                );

                let is_proxy_client = host_origin.proxy_protocol == Some(ProxyProtocol::ProxyProtocolV2);
                tracing::info!(is_proxy_client, tunnel_id = %host_origin.tunnel_id, ?flow_path, "new UDP client");

                let mut exclusive_endpoint = Some(TunnelEndpoint {
                    tunnel_id: host_origin.tunnel_id,
                    tunnel_flow: match flow_path {
                        UdpFlow::V4 { src, dst } => TunnelFlow::V4Client { tunnel_ip: *dst.ip(), client_ip: *src.ip(), client_port: src.port() },
                        UdpFlow::V6 { src, dst, .. } => TunnelFlow::V6Client { tunnel_ip: dst.0, client_ip: src.0, client_port: src.1 },
                    },
                    host_origin: host_origin.host_addr,
                    from_port: found.from_port,
                    to_port: found.to_port,
                });

                /*
                 * Search through sockets to see which is viable for client.
                 */

                let mut socket_id = None;

                for socket in self.sockets.iter_mut() {
                    /* cannot assign to tunnel socket */
                    if socket.socket_type != SocketType::Client {
                        continue;
                    }

                    match socket.endpoints.add::<TunnelEndpoint>(exclusive_endpoint.take().unwrap()) {
                        Ok(_) => {
                            socket_id = Some(socket.id);
                            break;
                        }
                        Err(v) => {
                            exclusive_endpoint = Some(v);
                        }
                    }
                }

                let socket_id = if let Some(id) = socket_id {
                    id
                } else {
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
                        endpoints: NonOverlapping::with(exclusive_endpoint.unwrap()),
                        socket_type: SocketType::Client,
                    };

                    tokio::spawn(UdpReceiverTask {
                        id: socket.id,
                        rx: socket.packet_io.clone(),
                        run: socket.run_receiver.clone(),
                        packets: self.packets.clone(),
                        tx: self.rx_packets_sender.clone(),
                        rx_offset: UDP_PROXY_PROTOCOL_MAX_LEN,
                    }.start());

                    socket_entry.insert(socket)
                };

                now = Instant::now();
                
                v.insert(Flow {
                    socket_id,
                    target_addr,
                    use_proxy_protocol: is_proxy_client,
                    last_client_packet: now,
                    last_host_packet: now,
                })
            }
        };

        flow.last_client_packet = now;

        let socket = self.sockets.get(flow.socket_id)
            .expect("could not load socket from id");

        if flow.use_proxy_protocol {
            match flow_path {
                UdpFlow::V4 { src, dst } => {
                    assert!(UDP_PROXY_PROTOCOL_LEN_V4 <= data_start);
                    let mut header_buffer = &mut buffer[data_start - UDP_PROXY_PROTOCOL_LEN_V4..];

                    ProxyProtocolHeader::AfInet {
                        client_ip: *src.ip(),
                        proxy_ip: *dst.ip(),
                        client_port: src.port(),
                        proxy_port: dst.port(),
                    }.write_v2_udp(&mut header_buffer).unwrap();

                    data_start -= UDP_PROXY_PROTOCOL_LEN_V4;
                    data_len += UDP_PROXY_PROTOCOL_LEN_V4;
                }
                UdpFlow::V6 { src, dst, .. } => {
                    assert!(UDP_PROXY_PROTOCOL_LEN_V6 <= data_start);
                    let mut header_buffer = &mut buffer[data_start - UDP_PROXY_PROTOCOL_LEN_V6..];

                    ProxyProtocolHeader::AfInet6 {
                        client_ip: src.0.into(),
                        proxy_ip: dst.0.into(),
                        client_port: src.1,
                        proxy_port: dst.1,
                    }.write_v2_udp(&mut header_buffer).unwrap();

                    data_start -= UDP_PROXY_PROTOCOL_LEN_V6;
                    data_len += UDP_PROXY_PROTOCOL_LEN_V6;
                }
            }
        }

        if let Err(error) = socket.packet_io.send_to(&buffer[data_start..(data_start + data_len)], flow.target_addr).await {
            if self.errors.send.check() {
                tracing::error!(?error, "failed to send packet");
            }
        }
    }
}

#[derive(Debug)]
pub struct TunnelEndpoint {
    pub tunnel_id: Uuid,
    pub tunnel_flow: TunnelFlow,
    pub host_origin: SocketAddr,
    pub from_port: u16,
    pub to_port: u16,
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


impl NonOverlappingCheck for TunnelEndpoint {
    type Element = TunnelEndpoint;

    fn is_same(a: &Self::Element, b: &Self::Element) -> bool {
        a.tunnel_id == b.tunnel_id
    }

    fn is_overlapping(a: &Self::Element, b: &Self::Element) -> bool {
        if !a.host_origin.ip().eq(&b.host_origin.ip()) {
            return false;
        }

        let a_start = a.host_origin.port();
        let a_end = a_start + (a.to_port - a.from_port);

        let b_start = b.host_origin.port();
        let b_end = b_start + (b.to_port - b.from_port);

        a_start.max(b_start) <= a_end.min(b_end)
    }
}
