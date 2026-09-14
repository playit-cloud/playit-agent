//! End-to-end tests of the UDP tunnel against a fake tunnel server and a local origin,
//! both plain sockets on loopback.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    num::NonZeroU64,
    sync::Arc,
    time::{Duration, Instant},
};

use playit_agent_core::{
    control::{ControlCommand, ControlHandle},
    origin::{OriginIp, OriginLookup, OriginResource, OriginTarget},
    stats::AgentStats,
    udp::{UdpSettings, UdpTunnel},
};
use playit_agent_proto::{
    PortProto,
    control_messages::UdpChannelDetails,
    udp_proto::{UDP_CHANNEL_ESTABLISH_ID, UdpFlow, UdpFlowExtension},
};
use tokio::{
    net::UdpSocket,
    sync::mpsc,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);
const TUNNEL_ID: u64 = 42;

struct Harness {
    tunnel_server: UdpSocket,
    origin_server: UdpSocket,
    channel_addr: SocketAddr,
    /// Held open so the tunnel keeps listening for session updates.
    _sessions: mpsc::Sender<UdpChannelDetails>,
    /// Held open so session requests from the tunnel have somewhere to go.
    _control_rx: mpsc::Receiver<ControlCommand>,
    stats: AgentStats,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

async fn start(origin_ip: IpAddr, settings: UdpSettings) -> Harness {
    let origin_server = UdpSocket::bind(SocketAddr::new(origin_ip, 0))
        .await
        .expect("bind origin server");
    start_with_origin(origin_server, 0, settings).await
}

async fn start_with_origin(
    origin_server: UdpSocket,
    port_count: u16,
    settings: UdpSettings,
) -> Harness {
    let tunnel_server = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind tunnel server");
    let tunnel_addr = tunnel_server.local_addr().unwrap();
    let origin_addr = origin_server.local_addr().unwrap();

    let lookup = Arc::new(OriginLookup::default());
    lookup.update([OriginResource {
        tunnel_id: TUNNEL_ID,
        proto: PortProto::Udp,
        target: OriginTarget::Port {
            ip: OriginIp::IpAddress(origin_addr.ip()),
            port: origin_addr.port(),
        },
        port_count,
        proxy_protocol: None,
    }]);

    let stats = AgentStats::new();
    let (control_tx, control_rx) = mpsc::channel(16);
    let tunnel = UdpTunnel::new(
        settings,
        lookup,
        stats.clone(),
        ControlHandle::new(control_tx),
    )
    .await
    .expect("create udp tunnel");

    let (sessions, session_rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(tunnel.run(session_rx, cancel.clone()));

    sessions
        .send(UdpChannelDetails {
            tunnel_addr,
            token: Arc::new(b"test-session-token".to_vec()),
        })
        .await
        .unwrap();

    let (token, channel_addr) = recv_from(&tunnel_server).await;
    assert_eq!(token, b"test-session-token");
    tunnel_server
        .send_to(&UDP_CHANNEL_ESTABLISH_ID.to_be_bytes(), channel_addr)
        .await
        .unwrap();

    Harness {
        tunnel_server,
        origin_server,
        channel_addr,
        _sessions: sessions,
        _control_rx: control_rx,
        stats,
        cancel,
        task,
    }
}

fn quick_eviction() -> UdpSettings {
    UdpSettings {
        flow_idle_timeout: Duration::from_millis(400),
        flow_one_way_timeout: Duration::from_millis(700),
        ..UdpSettings::default()
    }
}

fn flow_with_source(src_ip: Ipv4Addr, src_port: u16) -> UdpFlow {
    UdpFlow::V4 {
        src: SocketAddrV4::new(src_ip, src_port),
        dst: SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 7), 25_565),
        frag: None,
        extension: Some(UdpFlowExtension {
            client_server_id: NonZeroU64::new(7).unwrap(),
            tunnel_id: NonZeroU64::new(TUNNEL_ID).unwrap(),
            port_offset: 0,
        }),
    }
}

fn test_flow() -> UdpFlow {
    flow_with_source(Ipv4Addr::new(198, 51, 100, 10), 41_000)
}

fn with_port_offset(mut flow: UdpFlow, port_offset: u16) -> UdpFlow {
    let UdpFlow::V4 {
        dst,
        extension: Some(extension),
        ..
    } = &mut flow
    else {
        panic!("v4 flow with extension");
    };
    *dst = SocketAddrV4::new(*dst.ip(), dst.port() + port_offset);
    extension.port_offset = port_offset;
    flow
}

fn with_client_server_id(mut flow: UdpFlow, id: u64) -> UdpFlow {
    flow.update_client_server_id(NonZeroU64::new(id).unwrap());
    flow
}

async fn send_tunneled(socket: &UdpSocket, target: SocketAddr, flow: UdpFlow, payload: &[u8]) {
    let mut packet = payload.to_vec();
    packet.resize(payload.len() + flow.footer_len(), 0);
    assert!(flow.write_to(&mut packet[payload.len()..]));
    socket
        .send_to(&packet, target)
        .await
        .expect("send tunneled");
}

async fn recv_tunneled(socket: &UdpSocket) -> (UdpFlow, Vec<u8>, SocketAddr) {
    let (bytes, from) = recv_from(socket).await;
    let flow = UdpFlow::from_tail(&bytes).expect("flow footer");
    let payload = bytes[..bytes.len() - flow.footer_len()].to_vec();
    (flow, payload, from)
}

async fn recv_from(socket: &UdpSocket) -> (Vec<u8>, SocketAddr) {
    let mut buffer = vec![0u8; 2048];
    let (len, from) = timeout(TEST_TIMEOUT, socket.recv_from(&mut buffer))
        .await
        .expect("udp receive timeout")
        .expect("udp receive");
    buffer.truncate(len);
    (buffer, from)
}

async fn wait_for<F: FnMut() -> bool>(mut condition: F, limit: Duration) {
    let deadline = Instant::now() + limit;
    while !condition() {
        assert!(Instant::now() < deadline, "condition not met in {limit:?}");
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn relays_both_directions_and_survives_flow_eviction() {
    let h = start(IpAddr::V4(Ipv4Addr::LOCALHOST), quick_eviction()).await;
    let flow = test_flow();

    send_tunneled(&h.tunnel_server, h.channel_addr, flow, b"to origin").await;
    let (payload, virtual_addr) = recv_from(&h.origin_server).await;
    assert_eq!(payload, b"to origin");
    assert!(virtual_addr.ip().is_loopback());
    assert_eq!(h.stats.active_udp(), 1);

    h.origin_server
        .send_to(b"reply", virtual_addr)
        .await
        .unwrap();
    let (reply_flow, reply, from) = recv_tunneled(&h.tunnel_server).await;
    assert_eq!(from, h.channel_addr);
    assert_eq!(reply_flow, flow.flip());
    assert_eq!(reply, b"reply");

    // The tunnel server can move the client to a different server id; the flow stays.
    let moved = with_client_server_id(flow, 8);
    send_tunneled(&h.tunnel_server, h.channel_addr, moved, b"after move").await;
    let (payload, addr_after_move) = recv_from(&h.origin_server).await;
    assert_eq!(payload, b"after move");
    assert_eq!(addr_after_move, virtual_addr);
    assert_eq!(h.stats.active_udp(), 1);

    h.origin_server
        .send_to(b"reply 2", virtual_addr)
        .await
        .unwrap();
    let (reply_flow, reply, _) = recv_tunneled(&h.tunnel_server).await;
    assert_eq!(reply_flow, moved.flip());
    assert_eq!(reply, b"reply 2");

    assert_eq!(
        h.stats.bytes_in(),
        b"to origin".len() as u64 + b"after move".len() as u64
    );
    assert_eq!(
        h.stats.bytes_out(),
        b"reply".len() as u64 + b"reply 2".len() as u64
    );

    wait_for(|| h.stats.active_udp() == 0, Duration::from_secs(3)).await;

    send_tunneled(&h.tunnel_server, h.channel_addr, flow, b"after evict").await;
    let (payload, virtual_addr_2) = recv_from(&h.origin_server).await;
    assert_eq!(payload, b"after evict");
    assert!(virtual_addr_2.ip().is_loopback());
    assert_eq!(h.stats.active_udp(), 1);

    h.origin_server
        .send_to(b"reply 3", virtual_addr_2)
        .await
        .unwrap();
    let (reply_flow, reply, _) = recv_tunneled(&h.tunnel_server).await;
    assert_eq!(reply_flow, flow.flip());
    assert_eq!(reply, b"reply 3");
}

#[tokio::test]
async fn supports_ipv6_origins() {
    let h = start(IpAddr::V6(Ipv6Addr::LOCALHOST), UdpSettings::default()).await;
    let flow = test_flow();

    send_tunneled(&h.tunnel_server, h.channel_addr, flow, b"v6 payload").await;
    let (payload, virtual_addr) = recv_from(&h.origin_server).await;
    assert_eq!(payload, b"v6 payload");
    assert!(virtual_addr.is_ipv6());

    h.origin_server
        .send_to(b"v6 reply", virtual_addr)
        .await
        .unwrap();
    let (reply_flow, reply, from) = recv_tunneled(&h.tunnel_server).await;
    assert_eq!(from, h.channel_addr);
    assert_eq!(reply_flow, flow.flip());
    assert_eq!(reply, b"v6 reply");
}

#[tokio::test]
async fn isolates_parallel_flows() {
    let h = start(IpAddr::V4(Ipv4Addr::LOCALHOST), UdpSettings::default()).await;
    let flows: Vec<UdpFlow> = (0..3u16)
        .map(|i| flow_with_source(Ipv4Addr::new(198, 51, 100, 10 + i as u8), 41_000 + i))
        .collect();

    for (i, flow) in flows.iter().enumerate() {
        send_tunneled(
            &h.tunnel_server,
            h.channel_addr,
            *flow,
            format!("in {i}").as_bytes(),
        )
        .await;
    }

    let mut virtual_addrs: HashMap<usize, SocketAddr> = HashMap::new();
    for _ in 0..flows.len() {
        let (payload, from) = recv_from(&h.origin_server).await;
        let index: usize = std::str::from_utf8(&payload).unwrap()[3..].parse().unwrap();
        assert!(virtual_addrs.insert(index, from).is_none());
    }
    assert_eq!(h.stats.active_udp(), 3);

    let distinct: std::collections::HashSet<_> = virtual_addrs.values().collect();
    assert_eq!(distinct.len(), 3, "each client gets its own local socket");

    for (i, addr) in &virtual_addrs {
        h.origin_server
            .send_to(format!("out {i}").as_bytes(), addr)
            .await
            .unwrap();
    }

    let mut seen = Vec::new();
    for _ in 0..flows.len() {
        let (reply_flow, payload, _) = recv_tunneled(&h.tunnel_server).await;
        let index: usize = std::str::from_utf8(&payload).unwrap()[4..].parse().unwrap();
        assert_eq!(reply_flow, flows[index].flip());
        seen.push(index);
    }
    seen.sort_unstable();
    assert_eq!(seen, vec![0, 1, 2]);
}

#[tokio::test]
async fn different_port_offsets_from_one_client_are_separate_flows() {
    // Two origin sockets on consecutive ports so port_offset 1 has somewhere to go.
    let (origin_base, origin_next) = bind_consecutive_pair().await;
    let h = start_with_origin(origin_base, 2, UdpSettings::default()).await;
    let base_flow = test_flow();
    let offset_flow = with_port_offset(base_flow, 1);

    send_tunneled(&h.tunnel_server, h.channel_addr, base_flow, b"port 0").await;
    let (payload, addr_0) = recv_from(&h.origin_server).await;
    assert_eq!(payload, b"port 0");

    send_tunneled(&h.tunnel_server, h.channel_addr, offset_flow, b"port 1").await;
    let (payload, addr_1) = recv_from(&origin_next).await;
    assert_eq!(payload, b"port 1");
    assert_eq!(h.stats.active_udp(), 2);

    origin_next.send_to(b"from port 1", addr_1).await.unwrap();
    let (reply_flow, payload, _) = recv_tunneled(&h.tunnel_server).await;
    assert_eq!(payload, b"from port 1");
    assert_eq!(reply_flow, offset_flow.flip());

    h.origin_server
        .send_to(b"from port 0", addr_0)
        .await
        .unwrap();
    let (reply_flow, payload, _) = recv_tunneled(&h.tunnel_server).await;
    assert_eq!(payload, b"from port 0");
    assert_eq!(reply_flow, base_flow.flip());
}

#[tokio::test]
async fn asks_control_for_a_session_until_confirmed() {
    let tunnel_server = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let tunnel_addr = tunnel_server.local_addr().unwrap();

    let (control_tx, mut control_rx) = mpsc::channel(16);
    let tunnel = UdpTunnel::new(
        UdpSettings::default(),
        Arc::new(OriginLookup::default()),
        AgentStats::new(),
        ControlHandle::new(control_tx),
    )
    .await
    .unwrap();
    let (sessions, session_rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(tunnel.run(session_rx, cancel.clone()));

    // No session at all: request immediately.
    let first = timeout(Duration::from_secs(2), control_rx.recv())
        .await
        .unwrap();
    assert_eq!(first, Some(ControlCommand::RequestUdpSession));

    // Session provided: the token is sent, and once confirmed no further requests.
    sessions
        .send(UdpChannelDetails {
            tunnel_addr,
            token: Arc::new(b"tok".to_vec()),
        })
        .await
        .unwrap();
    let (token, channel_addr) = recv_from(&tunnel_server).await;
    assert_eq!(token, b"tok");
    tunnel_server
        .send_to(&UDP_CHANNEL_ESTABLISH_ID.to_be_bytes(), channel_addr)
        .await
        .unwrap();

    // Drain any request that raced with the confirmation, then expect silence.
    sleep(Duration::from_millis(100)).await;
    while control_rx.try_recv().is_ok() {}
    assert!(
        timeout(Duration::from_millis(1500), control_rx.recv())
            .await
            .is_err()
    );

    cancel.cancel();
    let _ = task.await;
}

#[tokio::test]
async fn drops_datagrams_from_unexpected_sources() {
    let h = start(IpAddr::V4(Ipv4Addr::LOCALHOST), UdpSettings::default()).await;
    let impostor = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();

    send_tunneled(&impostor, h.channel_addr, test_flow(), b"spoofed").await;
    send_tunneled(&h.tunnel_server, h.channel_addr, test_flow(), b"genuine").await;

    let (payload, _) = recv_from(&h.origin_server).await;
    assert_eq!(payload, b"genuine");
    assert_eq!(h.stats.udp().unexpected_source.get(), 1);
    assert_eq!(h.stats.active_udp(), 1);
}

async fn bind_consecutive_pair() -> (UdpSocket, UdpSocket) {
    for _ in 0..20 {
        let first = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = first.local_addr().unwrap().port();
        if let Ok(second) = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port + 1)).await
        {
            return (first, second);
        }
    }
    panic!("could not bind two consecutive udp ports");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "throughput report; run with -- --ignored --nocapture"]
async fn throughput_by_packet_size() {
    const PACKETS: usize = 100_000;
    const BATCH: usize = 32;

    let h = start(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        UdpSettings {
            packet_pool_size: 4096,
            ..UdpSettings::default()
        },
    )
    .await;
    let flow = test_flow();

    send_tunneled(&h.tunnel_server, h.channel_addr, flow, b"warmup").await;
    let (_, virtual_addr) = recv_from(&h.origin_server).await;

    for size in [32usize, 128, 512, 1300] {
        let payload = vec![size as u8; size];

        let start = Instant::now();
        let mut done = 0;
        while done < PACKETS {
            let batch = BATCH.min(PACKETS - done);
            for _ in 0..batch {
                send_tunneled(&h.tunnel_server, h.channel_addr, flow, &payload).await;
            }
            for _ in 0..batch {
                let (received, _) = recv_from(&h.origin_server).await;
                assert_eq!(received.len(), size);
            }
            done += batch;
        }
        let inbound = start.elapsed();

        let start = Instant::now();
        let mut done = 0;
        while done < PACKETS {
            let batch = BATCH.min(PACKETS - done);
            for _ in 0..batch {
                h.origin_server
                    .send_to(&payload, virtual_addr)
                    .await
                    .unwrap();
            }
            for _ in 0..batch {
                let (_, received, _) = recv_tunneled(&h.tunnel_server).await;
                assert_eq!(received.len(), size);
            }
            done += batch;
        }
        let outbound = start.elapsed();

        let mbps = |elapsed: Duration| (PACKETS * size) as f64 * 8.0 / elapsed.as_secs_f64() / 1e6;
        println!(
            "size={size}B tunnel->origin {:.1} Mbps ({:?}) origin->tunnel {:.1} Mbps ({:?})",
            mbps(inbound),
            inbound,
            mbps(outbound),
            outbound
        );
    }

    let _ = SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0);
}
