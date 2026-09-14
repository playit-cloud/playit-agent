//! End-to-end tests of TCP tunnels: a fake claim server stands in for the tunnel
//! server and a local listener stands in for the origin.

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::{Duration, Instant},
};

use playit_agent_core::{
    origin::{OriginIp, OriginLookup, OriginResource, OriginTarget},
    stats::AgentStats,
    tcp::{TcpSettings, TcpTunnels},
};
use playit_agent_proto::{
    PortProto,
    control_feed::{ClaimInstructions, NewClient},
};
use playit_api_client::api::ProxyProtocol;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

const TUNNEL_ID: u64 = 42;
const TOKEN: &[u8] = b"claim-token-123";
const TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Accepts one connection, checks the claim token, acks it, and hands the stream over.
async fn claim_server() -> (SocketAddr, oneshot::Receiver<TcpStream>) {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut token = vec![0u8; TOKEN.len()];
        stream.read_exact(&mut token).await.unwrap();
        assert_eq!(token, TOKEN);
        stream.write_all(&[0u8; 8]).await.unwrap();
        let _ = tx.send(stream);
    });

    (addr, rx)
}

/// Echoes until EOF, then writes `farewell` and closes.
async fn echo_origin(farewell: &'static [u8]) -> SocketAddr {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buffer = [0u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    stream.write_all(&buffer[..read]).await.unwrap();
                }
                stream.write_all(farewell).await.unwrap();
                let _ = stream.shutdown().await;
            });
        }
    });

    addr
}

fn lookup_for(origin: SocketAddr, proxy_protocol: Option<ProxyProtocol>) -> Arc<OriginLookup> {
    let lookup = Arc::new(OriginLookup::default());
    lookup.update([OriginResource {
        tunnel_id: TUNNEL_ID,
        proto: PortProto::Tcp,
        target: OriginTarget::Port {
            ip: OriginIp::IpAddress(origin.ip()),
            port: origin.port(),
        },
        port_count: 0,
        proxy_protocol,
    }]);
    lookup
}

fn new_client(claim: SocketAddr) -> NewClient {
    NewClient {
        connect_addr: "203.0.113.5:25565".parse().unwrap(),
        peer_addr: "198.51.100.10:41000".parse().unwrap(),
        data_center_id: 1,
        tunnel_id: TUNNEL_ID,
        port_offset: 0,
        claim_instructions: ClaimInstructions {
            address: claim,
            token: TOKEN.to_vec(),
        },
    }
}

async fn wait_for<F: FnMut() -> bool>(mut condition: F, limit: Duration) {
    let deadline = Instant::now() + limit;
    while !condition() {
        assert!(Instant::now() < deadline, "condition not met in {limit:?}");
        sleep(Duration::from_millis(20)).await;
    }
}

async fn read_to_end(stream: &mut TcpStream) -> Vec<u8> {
    let mut out = Vec::new();
    timeout(TEST_TIMEOUT, stream.read_to_end(&mut out))
        .await
        .unwrap()
        .unwrap();
    out
}

#[tokio::test]
async fn claims_connects_and_pipes_both_ways() {
    let origin = echo_origin(b"").await;
    let (claim_addr, claimed) = claim_server().await;
    let stats = AgentStats::new();
    let tunnels = TcpTunnels::new(
        TcpSettings::default(),
        lookup_for(origin, None),
        stats.clone(),
        CancellationToken::new(),
    );

    tunnels.handle_new_client(new_client(claim_addr));
    let mut client = timeout(TEST_TIMEOUT, claimed).await.unwrap().unwrap();

    client.write_all(b"ping").await.unwrap();
    let mut buffer = [0u8; 4];
    timeout(TEST_TIMEOUT, client.read_exact(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buffer, b"ping");

    wait_for(|| stats.active_tcp() == 1, TEST_TIMEOUT).await;
    let details = tunnels.connections();
    assert_eq!(details.len(), 1);
    assert_eq!(details[0].tunnel_id, TUNNEL_ID);
    assert_eq!(details[0].origin_addr, origin);
    assert_eq!(details[0].peer_addr, new_client(claim_addr).peer_addr);
    assert_eq!(details[0].bytes_to_origin, 4);
    assert_eq!(details[0].bytes_to_tunnel, 4);
    assert_eq!(stats.bytes_in(), 4);
    assert_eq!(stats.bytes_out(), 4);

    client.shutdown().await.unwrap();
    assert_eq!(read_to_end(&mut client).await, b"");
    wait_for(|| stats.active_tcp() == 0, TEST_TIMEOUT).await;
    assert!(tunnels.connections().is_empty());
}

#[tokio::test]
async fn forwards_half_close_and_keeps_other_direction_open() {
    let origin = echo_origin(b"farewell").await;
    let (claim_addr, claimed) = claim_server().await;
    let tunnels = TcpTunnels::new(
        TcpSettings::default(),
        lookup_for(origin, None),
        AgentStats::new(),
        CancellationToken::new(),
    );

    tunnels.handle_new_client(new_client(claim_addr));
    let mut client = timeout(TEST_TIMEOUT, claimed).await.unwrap().unwrap();

    client.write_all(b"hello").await.unwrap();
    client.shutdown().await.unwrap();

    // The origin only writes "farewell" after it sees EOF, so receiving it proves the
    // half-close was forwarded and the origin -> tunnel direction stayed open.
    assert_eq!(read_to_end(&mut client).await, b"hellofarewell");
}

#[tokio::test]
async fn idle_connections_are_closed() {
    let origin = echo_origin(b"").await;
    let (claim_addr, claimed) = claim_server().await;
    let stats = AgentStats::new();
    let tunnels = TcpTunnels::new(
        TcpSettings {
            idle_timeout: Duration::from_millis(300),
            ..TcpSettings::default()
        },
        lookup_for(origin, None),
        stats.clone(),
        CancellationToken::new(),
    );

    tunnels.handle_new_client(new_client(claim_addr));
    let mut client = timeout(TEST_TIMEOUT, claimed).await.unwrap().unwrap();
    wait_for(|| stats.active_tcp() == 1, TEST_TIMEOUT).await;

    let started = Instant::now();
    wait_for(|| stats.active_tcp() == 0, TEST_TIMEOUT).await;
    assert!(Duration::from_millis(250) <= started.elapsed());

    let mut buffer = [0u8; 1];
    let read = timeout(TEST_TIMEOUT, client.read(&mut buffer))
        .await
        .unwrap();
    assert!(
        matches!(read, Ok(0) | Err(_)),
        "client sees the close: {read:?}"
    );
}

#[tokio::test]
async fn writes_proxy_protocol_v1_header_before_data() {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let origin = listener.local_addr().unwrap();
    let (header_tx, header_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        stream.read_to_end(&mut received).await.unwrap();
        let _ = header_tx.send(received);
    });

    let (claim_addr, claimed) = claim_server().await;
    let tunnels = TcpTunnels::new(
        TcpSettings::default(),
        lookup_for(origin, Some(ProxyProtocol::ProxyProtocolV1)),
        AgentStats::new(),
        CancellationToken::new(),
    );

    tunnels.handle_new_client(new_client(claim_addr));
    let mut client = timeout(TEST_TIMEOUT, claimed).await.unwrap().unwrap();
    client.write_all(b"payload").await.unwrap();
    client.shutdown().await.unwrap();

    let received = timeout(TEST_TIMEOUT, header_rx).await.unwrap().unwrap();
    assert_eq!(
        received,
        b"PROXY TCP4 198.51.100.10 203.0.113.5 41000 25565\r\npayload"
    );
}

#[tokio::test]
async fn rate_limits_new_clients() {
    let stats = AgentStats::new();
    let tunnels = TcpTunnels::new(
        TcpSettings {
            new_client_ratelimit: 1,
            new_client_ratelimit_burst: 1,
            ..TcpSettings::default()
        },
        Arc::new(OriginLookup::default()),
        stats.clone(),
        CancellationToken::new(),
    );

    let unreachable_claim = "127.0.0.1:1".parse().unwrap();
    tunnels.handle_new_client(new_client(unreachable_claim));
    tunnels.handle_new_client(new_client(unreachable_claim));

    assert_eq!(stats.tcp().rate_limited.get(), 1);
    wait_for(|| stats.tcp().origin_not_found.get() == 1, TEST_TIMEOUT).await;
}

#[tokio::test]
async fn dropping_tunnels_closes_connections() {
    let origin = echo_origin(b"").await;
    let (claim_addr, claimed) = claim_server().await;
    let stats = AgentStats::new();
    let tunnels = TcpTunnels::new(
        TcpSettings::default(),
        lookup_for(origin, None),
        stats.clone(),
        CancellationToken::new(),
    );

    tunnels.handle_new_client(new_client(claim_addr));
    let mut client = timeout(TEST_TIMEOUT, claimed).await.unwrap().unwrap();
    wait_for(|| stats.active_tcp() == 1, TEST_TIMEOUT).await;

    drop(tunnels);

    let mut buffer = [0u8; 1];
    let read = timeout(TEST_TIMEOUT, client.read(&mut buffer))
        .await
        .unwrap();
    assert!(
        matches!(read, Ok(0) | Err(_)),
        "client sees the close: {read:?}"
    );
}
