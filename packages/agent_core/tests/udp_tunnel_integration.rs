use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddrV4},
    num::NonZeroU64,
    sync::Arc,
    time::Duration,
};

use futures_util::future::join_all;
use playit_agent_core::{
    network::{
        origin_lookup::{OriginIp, OriginLookup, OriginResource, OriginTarget},
        udp::{
            packets::Packets, udp_channel::UdpChannel, udp_clients::UdpClients,
            udp_settings::UdpSettings,
        },
    },
    stats::AgentStats,
};
use playit_agent_proto::{
    PortProto,
    control_messages::UdpChannelDetails,
    udp_proto::{UDP_CHANNEL_ESTABLISH_ID, UdpFlow, UdpFlowExtension},
};
use tokio::{net::UdpSocket, time::timeout};

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

#[tokio::test]
async fn encapsulated_udp_tunnel_relays_in_both_directions_and_recovers_same_flow_after_clear() {
    let tunnel_server = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind tunnel server");
    let origin_server = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind origin server");

    let tunnel_addr = tunnel_server.local_addr().expect("tunnel addr");
    let origin_addr = origin_server.local_addr().expect("origin addr");

    let lookup = Arc::new(OriginLookup::default());
    lookup
        .update(std::iter::once(OriginResource {
            tunnel_id: 42,
            proto: PortProto::Udp,
            target: OriginTarget::Port {
                ip: OriginIp::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port: origin_addr.port(),
            },
            port_count: 0,
            proxy_protocol: None,
        }))
        .await;

    let stats = AgentStats::new();
    let mut udp_clients = UdpClients::new(
        UdpSettings::default(),
        lookup,
        Packets::new(64),
        stats.clone(),
    );
    let mut udp_channel = UdpChannel::new(Packets::new(64))
        .await
        .expect("create udp channel");

    udp_channel
        .update_session(UdpChannelDetails {
            tunnel_addr,
            token: Arc::new(b"test-session-token".to_vec()),
        })
        .await;

    let (token_len, channel_addr, token_bytes) = recv_from_socket(&tunnel_server).await;
    assert_eq!(&token_bytes[..token_len], b"test-session-token");

    tunnel_server
        .send_to(&UDP_CHANNEL_ESTABLISH_ID.to_be_bytes(), channel_addr)
        .await
        .expect("send establish ack");

    let flow = test_flow();

    let origin_payload_1 = b"packet to local origin";
    send_tunneled_packet(&tunnel_server, channel_addr, flow, origin_payload_1).await;

    let (recv_flow_1, recv_packet_1) = timeout(TEST_TIMEOUT, udp_channel.recv())
        .await
        .expect("recv tunneled packet");
    assert_eq!(recv_flow_1, flow);
    udp_clients
        .handle_tunneled_packet(1_000, recv_flow_1, recv_packet_1)
        .await;

    let (origin_len_1, virtual_addr_1, origin_bytes_1) = recv_from_socket(&origin_server).await;
    assert_eq!(&origin_bytes_1[..origin_len_1], origin_payload_1);
    assert_eq!(stats.active_udp(), 1);

    let tunnel_reply_1 = b"reply from origin";
    origin_server
        .send_to(tunnel_reply_1, virtual_addr_1)
        .await
        .expect("origin send reply");

    let reply_1 = timeout(TEST_TIMEOUT, udp_clients.recv_origin_packet())
        .await
        .expect("recv origin reply");
    let (reply_flow_1, reply_packet_1) = udp_clients
        .dispatch_origin_packet(2_000, reply_1)
        .await
        .expect("dispatch origin reply");
    udp_channel.send(reply_flow_1, reply_packet_1).await;

    let (encap_flow_1, encap_payload_1, encap_source_1) =
        recv_tunneled_packet(&tunnel_server).await;
    assert_eq!(encap_source_1, channel_addr);
    assert_eq!(encap_flow_1, flow.flip());
    assert_eq!(encap_payload_1, tunnel_reply_1);

    udp_clients.clear_old(100_000).await;
    assert_eq!(stats.active_udp(), 0);

    let origin_payload_2 = b"packet after clear";
    send_tunneled_packet(&tunnel_server, channel_addr, flow, origin_payload_2).await;

    let (recv_flow_2, recv_packet_2) = timeout(TEST_TIMEOUT, udp_channel.recv())
        .await
        .expect("recv tunneled packet after clear");
    assert_eq!(recv_flow_2, flow);
    udp_clients
        .handle_tunneled_packet(101_000, recv_flow_2, recv_packet_2)
        .await;

    let (origin_len_2, virtual_addr_2, origin_bytes_2) = recv_from_socket(&origin_server).await;
    assert_eq!(&origin_bytes_2[..origin_len_2], origin_payload_2);
    assert!(virtual_addr_2.ip().is_loopback());
    assert_eq!(stats.active_udp(), 1);

    let tunnel_reply_2 = b"reply after clear";
    origin_server
        .send_to(tunnel_reply_2, virtual_addr_2)
        .await
        .expect("origin send second reply");

    let reply_2 = timeout(TEST_TIMEOUT, udp_clients.recv_origin_packet())
        .await
        .expect("recv origin reply after clear");
    let (reply_flow_2, reply_packet_2) = udp_clients
        .dispatch_origin_packet(102_000, reply_2)
        .await
        .expect("dispatch origin reply after clear");
    udp_channel.send(reply_flow_2, reply_packet_2).await;

    let (encap_flow_2, encap_payload_2, encap_source_2) =
        recv_tunneled_packet(&tunnel_server).await;
    assert_eq!(encap_source_2, channel_addr);
    assert_eq!(encap_flow_2, flow.flip());
    assert_eq!(encap_payload_2, tunnel_reply_2);
}

#[tokio::test]
async fn encapsulated_udp_tunnel_isolates_multiple_parallel_flows_and_recovers_them_after_clear() {
    let tunnel_server = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind tunnel server");
    let origin_server = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind origin server");

    let tunnel_addr = tunnel_server.local_addr().expect("tunnel addr");
    let origin_addr = origin_server.local_addr().expect("origin addr");

    let lookup = Arc::new(OriginLookup::default());
    lookup
        .update(std::iter::once(OriginResource {
            tunnel_id: 42,
            proto: PortProto::Udp,
            target: OriginTarget::Port {
                ip: OriginIp::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port: origin_addr.port(),
            },
            port_count: 0,
            proxy_protocol: None,
        }))
        .await;

    let stats = AgentStats::new();
    let mut udp_clients = UdpClients::new(
        UdpSettings::default(),
        lookup,
        Packets::new(128),
        stats.clone(),
    );
    let mut udp_channel = UdpChannel::new(Packets::new(128))
        .await
        .expect("create udp channel");

    udp_channel
        .update_session(UdpChannelDetails {
            tunnel_addr,
            token: Arc::new(b"test-session-token".to_vec()),
        })
        .await;

    let (token_len, channel_addr, token_bytes) = recv_from_socket(&tunnel_server).await;
    assert_eq!(&token_bytes[..token_len], b"test-session-token");

    tunnel_server
        .send_to(&UDP_CHANNEL_ESTABLISH_ID.to_be_bytes(), channel_addr)
        .await
        .expect("send establish ack");

    let cases = vec![
        FlowCase::new(
            0,
            flow_with_source(Ipv4Addr::new(198, 51, 100, 10), 41_000),
            b"flow-0 inbound".to_vec(),
            b"flow-0 outbound".to_vec(),
        ),
        FlowCase::new(
            1,
            flow_with_source(Ipv4Addr::new(198, 51, 100, 11), 41_001),
            b"flow-1 inbound".to_vec(),
            b"flow-1 outbound".to_vec(),
        ),
        FlowCase::new(
            2,
            flow_with_source(Ipv4Addr::new(198, 51, 100, 12), 41_002),
            b"flow-2 inbound".to_vec(),
            b"flow-2 outbound".to_vec(),
        ),
    ];

    let first_virtual_addrs = drive_parallel_flows(
        &tunnel_server,
        &origin_server,
        channel_addr,
        &mut udp_channel,
        &mut udp_clients,
        &cases,
        1_000,
        2_000,
    )
    .await;

    assert_eq!(first_virtual_addrs.len(), cases.len());
    assert_eq!(stats.active_udp(), cases.len() as u32);
    assert_unique_virtual_addrs(&first_virtual_addrs);

    udp_clients.clear_old(100_000).await;
    assert_eq!(stats.active_udp(), 0);

    let second_virtual_addrs = drive_parallel_flows(
        &tunnel_server,
        &origin_server,
        channel_addr,
        &mut udp_channel,
        &mut udp_clients,
        &cases,
        101_000,
        102_000,
    )
    .await;

    assert_eq!(second_virtual_addrs.len(), cases.len());
    assert_unique_virtual_addrs(&second_virtual_addrs);
    assert_eq!(stats.active_udp(), cases.len() as u32);
}

fn test_flow() -> UdpFlow {
    flow_with_source(Ipv4Addr::new(198, 51, 100, 10), 41_000)
}

fn flow_with_source(src_ip: Ipv4Addr, src_port: u16) -> UdpFlow {
    UdpFlow::V4 {
        src: SocketAddrV4::new(src_ip, src_port),
        dst: SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 7), 25_565),
        frag: None,
        extension: Some(UdpFlowExtension {
            client_server_id: NonZeroU64::new(7).expect("nonzero client server id"),
            tunnel_id: NonZeroU64::new(42).expect("nonzero tunnel id"),
            port_offset: 0,
        }),
    }
}

struct FlowCase {
    id: usize,
    flow: UdpFlow,
    inbound_payload: Vec<u8>,
    outbound_payload: Vec<u8>,
}

impl FlowCase {
    fn new(id: usize, flow: UdpFlow, inbound_payload: Vec<u8>, outbound_payload: Vec<u8>) -> Self {
        Self {
            id,
            flow,
            inbound_payload,
            outbound_payload,
        }
    }
}

async fn drive_parallel_flows(
    tunnel_server: &UdpSocket,
    origin_server: &UdpSocket,
    channel_addr: std::net::SocketAddr,
    udp_channel: &mut UdpChannel,
    udp_clients: &mut UdpClients,
    cases: &[FlowCase],
    tunnel_ts: u64,
    origin_ts: u64,
) -> HashMap<usize, std::net::SocketAddr> {
    join_all(cases.iter().map(|case| {
        send_tunneled_packet(
            tunnel_server,
            channel_addr,
            case.flow,
            &case.inbound_payload,
        )
    }))
    .await;

    for _ in 0..cases.len() {
        let (recv_flow, recv_packet) = timeout(TEST_TIMEOUT, udp_channel.recv())
            .await
            .expect("recv tunneled packet");
        udp_clients
            .handle_tunneled_packet(tunnel_ts, recv_flow, recv_packet)
            .await;
    }

    let inbound_by_payload: HashMap<Vec<u8>, &FlowCase> = cases
        .iter()
        .map(|case| (case.inbound_payload.clone(), case))
        .collect();

    let mut virtual_addrs = HashMap::new();
    for _ in 0..cases.len() {
        let (len, virtual_addr, bytes) = recv_from_socket(origin_server).await;
        let payload = bytes[..len].to_vec();
        let case = inbound_by_payload
            .get(&payload)
            .expect("unexpected inbound payload at origin");
        assert_eq!(virtual_addrs.insert(case.id, virtual_addr), None);
    }

    join_all(cases.iter().map(|case| async {
        let virtual_addr = *virtual_addrs
            .get(&case.id)
            .expect("missing virtual addr for case");
        origin_server
            .send_to(&case.outbound_payload, virtual_addr)
            .await
            .expect("origin send reply");
    }))
    .await;

    for _ in 0..cases.len() {
        let reply = timeout(TEST_TIMEOUT, udp_clients.recv_origin_packet())
            .await
            .expect("recv origin reply");
        let (reply_flow, reply_packet) = udp_clients
            .dispatch_origin_packet(origin_ts, reply)
            .await
            .expect("dispatch origin reply");
        udp_channel.send(reply_flow, reply_packet).await;
    }

    let cases_by_outbound: HashMap<Vec<u8>, &FlowCase> = cases
        .iter()
        .map(|case| (case.outbound_payload.clone(), case))
        .collect();

    let mut seen_flow_ids = Vec::new();
    for _ in 0..cases.len() {
        let (encap_flow, encap_payload, encap_source) = recv_tunneled_packet(tunnel_server).await;
        assert_eq!(encap_source, channel_addr);

        let case = cases_by_outbound
            .get(&encap_payload)
            .expect("unexpected outbound payload at tunnel");
        assert_eq!(encap_flow, case.flow.flip());
        seen_flow_ids.push(case.id);
    }

    seen_flow_ids.sort_unstable();
    assert_eq!(
        seen_flow_ids,
        cases.iter().map(|case| case.id).collect::<Vec<_>>()
    );

    virtual_addrs
}

fn assert_unique_virtual_addrs(addrs: &HashMap<usize, std::net::SocketAddr>) {
    let mut seen = Vec::new();
    for addr in addrs.values().copied() {
        assert!(
            !seen.contains(&addr),
            "duplicate virtual client address observed: {addr}"
        );
        seen.push(addr);
    }
}

async fn send_tunneled_packet(
    socket: &UdpSocket,
    target: std::net::SocketAddr,
    flow: UdpFlow,
    payload: &[u8],
) {
    let mut packet = Vec::with_capacity(payload.len() + flow.footer_len());
    packet.extend_from_slice(payload);
    packet.resize(payload.len() + flow.footer_len(), 0);
    assert!(flow.write_to(&mut packet[payload.len()..]));

    socket
        .send_to(&packet, target)
        .await
        .expect("send tunneled packet");
}

async fn recv_tunneled_packet(socket: &UdpSocket) -> (UdpFlow, Vec<u8>, std::net::SocketAddr) {
    let (len, source, buf) = recv_from_socket(socket).await;
    let flow = UdpFlow::from_tail(&buf[..len]).expect("parse flow footer");
    let payload = buf[..len - flow.footer_len()].to_vec();
    (flow, payload, source)
}

async fn recv_from_socket(socket: &UdpSocket) -> (usize, std::net::SocketAddr, [u8; 2048]) {
    let mut buf = [0u8; 2048];
    let (len, source) = timeout(TEST_TIMEOUT, socket.recv_from(&mut buf))
        .await
        .expect("udp receive timeout")
        .expect("udp receive");
    (len, source, buf)
}
