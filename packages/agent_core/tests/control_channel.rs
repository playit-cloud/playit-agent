//! Exercises the control channel against a fake tunnel server speaking the real wire
//! protocol over loopback UDP, with a fake API that signs register requests.

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use message_encoding::MessageEncoding;
use playit_agent_core::{
    control::{ControlApi, ControlChannel, ControlEvent, ControlSettings},
    error::SetupError,
};
use playit_agent_proto::{
    AgentSessionId,
    control_feed::{ClaimInstructions, ControlFeed, NewClient},
    control_messages::{
        AgentRegister, AgentRegistered, ControlRequest, ControlResponse, Pong, UdpChannelDetails,
    },
    rpc::ControlRpcMessage,
};
use tokio::{
    net::UdpSocket,
    sync::mpsc,
    time::{Instant, timeout},
};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[derive(Default)]
struct ServerState {
    /// Number of register requests accepted.
    registers: AtomicU32,
    /// Session id the server currently considers valid (0 = none).
    session: AtomicU64,
    /// When false the server drops everything, simulating an outage.
    responsive: AtomicBool,
    /// Added to the client port reported in pongs, to simulate a NAT rebinding.
    client_port_shift: AtomicU16,
    keepalives: AtomicU32,
    udp_setup_requests: AtomicU32,
}

struct FakeServer {
    addr: SocketAddr,
    state: Arc<ServerState>,
    inject: mpsc::Sender<ControlFeed>,
}

impl FakeServer {
    async fn start() -> Self {
        let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = socket.local_addr().unwrap();
        let state = Arc::new(ServerState {
            responsive: AtomicBool::new(true),
            ..ServerState::default()
        });
        let (inject, inject_rx) = mpsc::channel(16);

        tokio::spawn(serve(socket, state.clone(), inject_rx));
        FakeServer {
            addr,
            state,
            inject,
        }
    }
}

async fn serve(
    socket: UdpSocket,
    state: Arc<ServerState>,
    mut inject: mpsc::Receiver<ControlFeed>,
) {
    let local = socket.local_addr().unwrap();
    let mut buffer = vec![0u8; 2048];
    let mut out = Vec::new();
    let mut agent_addr: Option<SocketAddr> = None;

    loop {
        tokio::select! {
            received = socket.recv_from(&mut buffer) => {
                let Ok((len, from)) = received else { continue };
                if !state.responsive.load(Ordering::Relaxed) {
                    continue;
                }
                agent_addr = Some(from);

                let Ok(request) = ControlRpcMessage::<ControlRequest>::read_from(&mut &buffer[..len]) else {
                    continue;
                };

                let mut seen_client = from;
                seen_client.set_port(from.port().wrapping_add(state.client_port_shift.load(Ordering::Relaxed)));

                let response = match request.content {
                    ControlRequest::Ping(ping) => {
                        let current = state.session.load(Ordering::Relaxed);
                        let known = ping.session_id.as_ref().is_some_and(|id| id.session_id == current && current != 0);
                        ControlResponse::Pong(Pong {
                            request_now: ping.now,
                            server_now: now_ms(),
                            server_id: 1,
                            data_center_id: 1,
                            client_addr: seen_client,
                            tunnel_addr: local,
                            session_expire_at: known.then(|| now_ms() + 60_000),
                        })
                    }
                    ControlRequest::AgentRegister(register) => {
                        if register.client_addr != seen_client || register.tunnel_addr != local {
                            ControlResponse::Unauthorized
                        } else {
                            let id = state.registers.fetch_add(1, Ordering::Relaxed) as u64 + 1;
                            state.session.store(id, Ordering::Relaxed);
                            ControlResponse::AgentRegistered(AgentRegistered {
                                id: session_id(id),
                                expires_at: now_ms() + 60_000,
                            })
                        }
                    }
                    ControlRequest::AgentKeepAlive(id) => {
                        state.keepalives.fetch_add(1, Ordering::Relaxed);
                        ControlResponse::AgentRegistered(AgentRegistered { id, expires_at: now_ms() + 60_000 })
                    }
                    ControlRequest::SetupUdpChannel(_) => {
                        state.udp_setup_requests.fetch_add(1, Ordering::Relaxed);
                        ControlResponse::UdpChannelDetails(UdpChannelDetails {
                            tunnel_addr: local,
                            token: Arc::new(vec![1, 2, 3, 4]),
                        })
                    }
                    _ => continue,
                };

                out.clear();
                ControlFeed::Response(ControlRpcMessage { request_id: request.request_id, content: response })
                    .write_to(&mut out)
                    .unwrap();
                let _ = socket.send_to(&out, from).await;
            }
            feed = inject.recv() => {
                let Some(feed) = feed else { break };
                let Some(agent) = agent_addr else { continue };
                out.clear();
                feed.write_to(&mut out).unwrap();
                let _ = socket.send_to(&out, agent).await;
            }
        }
    }
}

fn session_id(id: u64) -> AgentSessionId {
    AgentSessionId {
        session_id: id,
        account_id: 1,
        agent_id: 1,
    }
}

#[derive(Clone)]
struct FakeApi {
    addresses: Arc<std::sync::Mutex<Vec<SocketAddr>>>,
    sign_calls: Arc<AtomicU32>,
}

impl FakeApi {
    fn new(addr: SocketAddr) -> Self {
        FakeApi {
            addresses: Arc::new(std::sync::Mutex::new(vec![addr])),
            sign_calls: Arc::new(AtomicU32::new(0)),
        }
    }
}

impl ControlApi for FakeApi {
    async fn control_addresses(&self) -> Result<Vec<SocketAddr>, SetupError> {
        Ok(self.addresses.lock().unwrap().clone())
    }

    async fn sign_register(
        &self,
        client_addr: SocketAddr,
        tunnel_addr: SocketAddr,
    ) -> Result<Vec<u8>, SetupError> {
        self.sign_calls.fetch_add(1, Ordering::Relaxed);
        let mut out = Vec::new();
        ControlRequest::AgentRegister(AgentRegister {
            proto_version: 2,
            account_id: 1,
            agent_id: 1,
            agent_version: 1,
            timestamp: now_ms(),
            client_addr,
            tunnel_addr,
            signature: [0u8; 32],
        })
        .write_to(&mut out)
        .unwrap();
        Ok(out)
    }
}

fn fast_settings() -> ControlSettings {
    ControlSettings {
        ping_interval: Duration::from_millis(100),
        pong_timeout: Duration::from_millis(600),
        connect_timeout: Duration::from_secs(5),
        max_recovery_backoff: Duration::from_secs(1),
        ..ControlSettings::default()
    }
}

/// Runs the channel for `duration`, collecting events.
async fn drive(control: &mut ControlChannel<FakeApi>, duration: Duration) -> Vec<ControlEvent> {
    let deadline = Instant::now() + duration;
    let mut events = Vec::new();
    while let Ok(event) = tokio::time::timeout_at(deadline, control.next_event()).await {
        events.push(event);
    }
    events
}

async fn drive_until<F: FnMut() -> bool>(
    control: &mut ControlChannel<FakeApi>,
    limit: Duration,
    mut done: F,
) -> Vec<ControlEvent> {
    let deadline = Instant::now() + limit;
    let mut events = Vec::new();
    while !done() {
        assert!(
            Instant::now() < deadline,
            "condition not met within {limit:?}"
        );
        if let Ok(event) = timeout(Duration::from_millis(50), control.next_event()).await {
            events.push(event);
        }
    }
    events
}

fn new_client(claim: SocketAddr) -> NewClient {
    NewClient {
        connect_addr: "203.0.113.5:25565".parse().unwrap(),
        peer_addr: "198.51.100.10:41000".parse().unwrap(),
        data_center_id: 1,
        tunnel_id: 42,
        port_offset: 0,
        claim_instructions: ClaimInstructions {
            address: claim,
            token: vec![9, 9, 9],
        },
    }
}

#[tokio::test]
async fn connect_registers_and_keeps_session_alive() {
    let server = FakeServer::start().await;
    let api = FakeApi::new(server.addr);

    let mut control = ControlChannel::connect(api.clone(), fast_settings())
        .await
        .unwrap();
    assert_eq!(server.state.registers.load(Ordering::Relaxed), 1);
    assert_eq!(control.remote(), server.addr);
    assert_eq!(control.session().unwrap().id, session_id(1));

    let events = drive(&mut control, Duration::from_millis(500)).await;
    assert!(events.is_empty());
    assert!(control.session().is_some());
    assert!(control.rtt_ms().is_some(), "pongs were processed");
    assert_eq!(
        server.state.registers.load(Ordering::Relaxed),
        1,
        "a healthy session is not re-registered"
    );
    assert_eq!(api.sign_calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn new_client_feed_becomes_an_event() {
    let server = FakeServer::start().await;
    let mut control = ControlChannel::connect(FakeApi::new(server.addr), fast_settings())
        .await
        .unwrap();

    let client = new_client("127.0.0.1:9".parse().unwrap());
    server
        .inject
        .send(ControlFeed::NewClient(client.clone()))
        .await
        .unwrap();

    let event = timeout(Duration::from_secs(2), control.next_event())
        .await
        .unwrap();
    match event {
        ControlEvent::NewClient(received) => assert_eq!(received, client),
        other => panic!("unexpected event {other:?}"),
    }
}

#[tokio::test]
async fn udp_session_request_round_trips() {
    let server = FakeServer::start().await;
    let mut control = ControlChannel::connect(FakeApi::new(server.addr), fast_settings())
        .await
        .unwrap();

    control.handle().request_udp_session();
    control.handle().request_udp_session();

    let event = timeout(Duration::from_secs(2), control.next_event())
        .await
        .unwrap();
    match event {
        ControlEvent::UdpSession(details) => {
            assert_eq!(details.tunnel_addr, server.addr);
            assert_eq!(*details.token, vec![1, 2, 3, 4]);
        }
        other => panic!("unexpected event {other:?}"),
    }
    assert_eq!(
        server.state.udp_setup_requests.load(Ordering::Relaxed),
        1,
        "requests coalesce"
    );
}

#[tokio::test]
async fn unauthorized_response_triggers_reregistration() {
    let server = FakeServer::start().await;
    let mut control = ControlChannel::connect(FakeApi::new(server.addr), fast_settings())
        .await
        .unwrap();

    server
        .inject
        .send(ControlFeed::Response(ControlRpcMessage {
            request_id: 77,
            content: ControlResponse::Unauthorized,
        }))
        .await
        .unwrap();

    let state = server.state.clone();
    drive_until(&mut control, Duration::from_secs(3), || {
        state.registers.load(Ordering::Relaxed) == 2
    })
    .await;
    assert_eq!(control.session().unwrap().id, session_id(2));
}

#[tokio::test]
async fn server_forgetting_session_triggers_reregistration() {
    let server = FakeServer::start().await;
    let mut control = ControlChannel::connect(FakeApi::new(server.addr), fast_settings())
        .await
        .unwrap();
    drive(&mut control, Duration::from_millis(300)).await;

    // Server-side restart: it no longer knows any session, so pongs carry no expiry.
    server.state.session.store(0, Ordering::Relaxed);

    let state = server.state.clone();
    drive_until(&mut control, Duration::from_secs(3), || {
        state.registers.load(Ordering::Relaxed) == 2
    })
    .await;
    assert_eq!(state.session.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn observed_address_change_triggers_reregistration() {
    let server = FakeServer::start().await;
    let api = FakeApi::new(server.addr);
    let mut control = ControlChannel::connect(api.clone(), fast_settings())
        .await
        .unwrap();
    drive(&mut control, Duration::from_millis(300)).await;

    server.state.client_port_shift.store(7, Ordering::Relaxed);

    let state = server.state.clone();
    drive_until(&mut control, Duration::from_secs(3), || {
        state.registers.load(Ordering::Relaxed) == 2
    })
    .await;

    let session = control.session().unwrap();
    assert_eq!(session.id, session_id(2));
    assert_eq!(
        session.client_addr.port(),
        control.latest_pong().client_addr.port(),
        "new session is signed for the shifted address"
    );
}

#[tokio::test]
async fn outage_triggers_reconnect_once_server_returns() {
    let server = FakeServer::start().await;
    let api = FakeApi::new(server.addr);
    let mut control = ControlChannel::connect(api.clone(), fast_settings())
        .await
        .unwrap();

    server.state.responsive.store(false, Ordering::Relaxed);
    drive(&mut control, Duration::from_millis(900)).await;
    assert!(
        control.session().is_none(),
        "session dropped after pong timeout"
    );

    server.state.responsive.store(true, Ordering::Relaxed);
    let state = server.state.clone();
    drive_until(&mut control, Duration::from_secs(8), || {
        state.registers.load(Ordering::Relaxed) == 2
    })
    .await;
    assert!(control.session().is_some());
}

#[tokio::test]
async fn connect_fails_when_no_server_answers() {
    let dead = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let api = FakeApi::new(dead.local_addr().unwrap());

    let error = ControlChannel::connect(api, fast_settings())
        .await
        .err()
        .unwrap();
    assert!(matches!(error, SetupError::NoControlResponse), "{error}");
}

#[tokio::test]
async fn connect_fails_without_addresses() {
    let api = FakeApi {
        addresses: Arc::new(std::sync::Mutex::new(Vec::new())),
        sign_calls: Arc::new(AtomicU32::new(0)),
    };

    let error = ControlChannel::connect(api, fast_settings())
        .await
        .err()
        .unwrap();
    assert!(matches!(error, SetupError::NoControlAddresses), "{error}");
}
