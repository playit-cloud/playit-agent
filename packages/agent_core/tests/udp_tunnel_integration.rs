use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6},
    num::NonZeroU64,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::future::join_all;
use playit_agent_core::{
    network::{
        origin_lookup::{OriginIp, OriginLookup, OriginResource, OriginTarget},
        udp::{
            packets::Packets, udp_channel::UdpChannel, udp_clients::UdpClients,
            udp_settings::UdpSettings,
        },
        upload_qos::UploadFairness,
    },
    stats::AgentStats,
};
use playit_agent_proto::{
    PortProto,
    control_messages::UdpChannelDetails,
    udp_proto::{UDP_CHANNEL_ESTABLISH_ID, UdpFlow, UdpFlowExtension},
};
use tokio::{net::UdpSocket, time::timeout};
use tokio_util::sync::CancellationToken;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);
const STRESS_TIMEOUT: Duration = Duration::from_secs(30);
const UDP_STRESS_PACKET_COUNT: usize = 100_000;
const UDP_STRESS_PACKET_SIZES: [usize; 4] = [32, 128, 512, 1300];
const UDP_STRESS_BATCH_SIZE: usize = 32;

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
    let cancel = CancellationToken::new();
    let upload_fairness = UploadFairness::new(cancel.child_token());
    let mut udp_clients = UdpClients::new(
        UdpSettings::default(),
        lookup,
        Packets::new(64),
        stats.clone(),
        cancel,
        upload_fairness,
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
async fn encapsulated_udp_tunnel_supports_ipv6_origin_addresses() {
    let tunnel_server = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind tunnel server");
    let origin_server = UdpSocket::bind(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0))
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
                ip: OriginIp::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)),
                port: origin_addr.port(),
            },
            port_count: 0,
            proxy_protocol: None,
        }))
        .await;

    let stats = AgentStats::new();
    let cancel = CancellationToken::new();
    let upload_fairness = UploadFairness::new(cancel.child_token());
    let mut udp_clients = UdpClients::new(
        UdpSettings::default(),
        lookup,
        Packets::new(64),
        stats.clone(),
        cancel,
        upload_fairness,
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
    let origin_payload = b"packet to ipv6 origin";
    send_tunneled_packet(&tunnel_server, channel_addr, flow, origin_payload).await;

    let (recv_flow, recv_packet) = timeout(TEST_TIMEOUT, udp_channel.recv())
        .await
        .expect("recv tunneled packet");
    assert_eq!(recv_flow, flow);
    udp_clients
        .handle_tunneled_packet(1_000, recv_flow, recv_packet)
        .await;

    let (origin_len, virtual_addr, origin_bytes) = recv_from_socket(&origin_server).await;
    assert_eq!(&origin_bytes[..origin_len], origin_payload);
    assert!(virtual_addr.is_ipv6());
    assert_eq!(stats.active_udp(), 1);

    let tunnel_reply = b"reply from ipv6 origin";
    origin_server
        .send_to(tunnel_reply, virtual_addr)
        .await
        .expect("origin send reply");

    let reply = timeout(TEST_TIMEOUT, udp_clients.recv_origin_packet())
        .await
        .expect("recv origin reply");
    let (reply_flow, reply_packet) = udp_clients
        .dispatch_origin_packet(2_000, reply)
        .await
        .expect("dispatch origin reply");
    udp_channel.send(reply_flow, reply_packet).await;

    let (encap_flow, encap_payload, encap_source) = recv_tunneled_packet(&tunnel_server).await;
    assert_eq!(encap_source, channel_addr);
    assert_eq!(encap_flow, flow.flip());
    assert_eq!(encap_payload, tunnel_reply);
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
    let cancel = CancellationToken::new();
    let upload_fairness = UploadFairness::new(cancel.child_token());
    let mut udp_clients = UdpClients::new(
        UdpSettings::default(),
        lookup,
        Packets::new(128),
        stats.clone(),
        cancel,
        upload_fairness,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "stress/perf test; run with -- --ignored --nocapture"]
async fn udp_tunnel_stress_reports_bitrate_by_packet_size() {
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
    let cancel = CancellationToken::new();
    let upload_fairness = UploadFairness::new(cancel.child_token());
    let mut udp_clients = UdpClients::new(
        UdpSettings::default(),
        lookup,
        Packets::new(4096),
        stats.clone(),
        cancel,
        upload_fairness,
    );
    let mut udp_channel = UdpChannel::new(Packets::new(4096))
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
    let virtual_addr = establish_virtual_client(
        &tunnel_server,
        &origin_server,
        channel_addr,
        &mut udp_channel,
        &mut udp_clients,
        flow,
    )
    .await;

    println!(
        "udp stress: packets_per_size={}, flow_footer_bytes={}",
        UDP_STRESS_PACKET_COUNT,
        flow.footer_len()
    );

    for size in UDP_STRESS_PACKET_SIZES {
        let inbound_payload = vec![size as u8; size];
        let outbound_payload = vec![(size as u8).wrapping_add(1); size];

        let inbound = measure_tunnel_to_origin_bitrate(
            &tunnel_server,
            &origin_server,
            channel_addr,
            virtual_addr,
            &mut udp_channel,
            &mut udp_clients,
            &stats,
            flow,
            &inbound_payload,
        )
        .await;

        let outbound = measure_origin_to_tunnel_bitrate(
            &origin_server,
            &tunnel_server,
            virtual_addr,
            channel_addr,
            &mut udp_channel,
            &mut udp_clients,
            &stats,
            flow,
            &outbound_payload,
        )
        .await;

        println!(
            "udp stress size={}B tunnel->origin payload_mbps={:.2} tunnel_wire_mbps={:.2} elapsed_ms={} origin->tunnel payload_mbps={:.2} tunnel_wire_mbps={:.2} elapsed_ms={}",
            size,
            inbound.payload_mbps,
            inbound.tunnel_wire_mbps,
            inbound.elapsed.as_millis(),
            outbound.payload_mbps,
            outbound.tunnel_wire_mbps,
            outbound.elapsed.as_millis(),
        );
    }
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

async fn establish_virtual_client(
    tunnel_server: &UdpSocket,
    origin_server: &UdpSocket,
    channel_addr: std::net::SocketAddr,
    udp_channel: &mut UdpChannel,
    udp_clients: &mut UdpClients,
    flow: UdpFlow,
) -> std::net::SocketAddr {
    let warmup_payload = b"udp-stress-warmup";
    send_tunneled_packet(tunnel_server, channel_addr, flow, warmup_payload).await;

    let (recv_flow, recv_packet) = timeout(TEST_TIMEOUT, udp_channel.recv())
        .await
        .expect("recv warmup tunneled packet");
    assert_eq!(recv_flow, flow);
    udp_clients
        .handle_tunneled_packet(1_000, recv_flow, recv_packet)
        .await;

    let (len, virtual_addr, bytes) = recv_from_socket(origin_server).await;
    assert_eq!(&bytes[..len], warmup_payload);
    virtual_addr
}

async fn measure_tunnel_to_origin_bitrate(
    tunnel_server: &UdpSocket,
    origin_server: &UdpSocket,
    channel_addr: std::net::SocketAddr,
    virtual_addr: std::net::SocketAddr,
    udp_channel: &mut UdpChannel,
    udp_clients: &mut UdpClients,
    stats: &AgentStats,
    flow: UdpFlow,
    payload: &[u8],
) -> ThroughputResult {
    let expected_bytes = UDP_STRESS_PACKET_COUNT * payload.len();
    let expected_tunnel_bytes = UDP_STRESS_PACKET_COUNT * (payload.len() + flow.footer_len());
    let before = stats.snapshot();
    let start = Instant::now();

    timeout(STRESS_TIMEOUT, async {
        let mut processed = 0usize;
        while processed < UDP_STRESS_PACKET_COUNT {
            let batch = UDP_STRESS_BATCH_SIZE.min(UDP_STRESS_PACKET_COUNT - processed);

            for _ in 0..batch {
                send_tunneled_packet(tunnel_server, channel_addr, flow, payload).await;
            }

            for i in 0..batch {
                let (recv_flow, recv_packet) = udp_channel.recv().await;
                assert_eq!(recv_flow, flow);
                udp_clients
                    .handle_tunneled_packet(10_000 + (processed + i) as u64, recv_flow, recv_packet)
                    .await;
            }

            for _ in 0..batch {
                let (len, source, bytes) = recv_from_socket(origin_server).await;
                assert_eq!(source, virtual_addr);
                assert_eq!(&bytes[..len], payload);
            }

            processed += batch;
        }
    })
    .await
    .expect("tunnel to origin stress timed out");

    let elapsed = start.elapsed();
    let after = stats.snapshot();
    assert_eq!(after.bytes_in - before.bytes_in, expected_bytes as u64);

    ThroughputResult::new(elapsed, expected_bytes, expected_tunnel_bytes)
}

async fn measure_origin_to_tunnel_bitrate(
    origin_server: &UdpSocket,
    tunnel_server: &UdpSocket,
    virtual_addr: std::net::SocketAddr,
    channel_addr: std::net::SocketAddr,
    udp_channel: &mut UdpChannel,
    udp_clients: &mut UdpClients,
    stats: &AgentStats,
    flow: UdpFlow,
    payload: &[u8],
) -> ThroughputResult {
    let expected_bytes = UDP_STRESS_PACKET_COUNT * payload.len();
    let expected_tunnel_bytes = UDP_STRESS_PACKET_COUNT * (payload.len() + flow.footer_len());
    let before = stats.snapshot();
    let start = Instant::now();

    timeout(STRESS_TIMEOUT, async {
        let mut processed = 0usize;
        while processed < UDP_STRESS_PACKET_COUNT {
            let batch = UDP_STRESS_BATCH_SIZE.min(UDP_STRESS_PACKET_COUNT - processed);

            for _ in 0..batch {
                origin_server
                    .send_to(payload, virtual_addr)
                    .await
                    .expect("origin send stress packet");
            }

            for i in 0..batch {
                let recv = udp_clients.recv_origin_packet().await;
                let (reply_flow, reply_packet) = udp_clients
                    .dispatch_origin_packet(20_000 + (processed + i) as u64, recv)
                    .await
                    .expect("dispatch origin stress packet");
                assert_eq!(reply_flow, flow.flip());
                udp_channel.send(reply_flow, reply_packet).await;
            }

            for _ in 0..batch {
                let (encap_flow, encap_payload, encap_source) =
                    recv_tunneled_packet(tunnel_server).await;
                assert_eq!(encap_source, channel_addr);
                assert_eq!(encap_flow, flow.flip());
                assert_eq!(encap_payload, payload);
            }

            processed += batch;
        }
    })
    .await
    .expect("origin to tunnel stress timed out");

    let elapsed = start.elapsed();
    let after = stats.snapshot();
    assert_eq!(after.bytes_out - before.bytes_out, expected_bytes as u64);

    ThroughputResult::new(elapsed, expected_bytes, expected_tunnel_bytes)
}

struct ThroughputResult {
    elapsed: Duration,
    payload_mbps: f64,
    tunnel_wire_mbps: f64,
}

impl ThroughputResult {
    fn new(elapsed: Duration, payload_bytes: usize, tunnel_wire_bytes: usize) -> Self {
        let seconds = elapsed.as_secs_f64().max(f64::EPSILON);

        Self {
            elapsed,
            payload_mbps: (payload_bytes as f64 * 8.0) / seconds / 1_000_000.0,
            tunnel_wire_mbps: (tunnel_wire_bytes as f64 * 8.0) / seconds / 1_000_000.0,
        }
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
