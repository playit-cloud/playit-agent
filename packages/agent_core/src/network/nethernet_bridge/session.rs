//! One player: their NetherNet connection, joined to a NetherNet connection the bridge opens
//! to the Bedrock server (`transport=nethernet`).
//!
//! Both ends are WebRTC data channels with the same NetherNet framing, so data channel messages
//! are copied between them as they are, never decoded. The player's login, including the
//! Xbox-signed identity, and the encryption handshake that follows reach the server untouched.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::change::SdpAnswer;
use str0m::channel::{ChannelConfig, ChannelId, Reliability};
use str0m::config::DtlsCert;
use str0m::crypto::Fingerprint;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, IceCreds, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::admission::Admitted;
use super::bds::{Signaling, SignalingError, SignalingIdentity, prepare_answer};
use super::stun::rewrite_username;
use crate::utils::now_sec;

pub const RELIABLE_CHANNEL: &str = "ReliableDataChannel";
pub const UNRELIABLE_CHANNEL: &str = "UnreliableDataChannel";

/// Bytes queued toward either peer beyond what SCTP accepted. Past this that peer is not
/// keeping up, and the session ends.
const MAX_PENDING: usize = 64 * 1024 * 1024;
const SCTP_BUFFER: usize = 4 * 1024 * 1024;

const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct SessionSetup {
    pub admitted: Admitted,
    /// Where the player's packets come from: the agent's per-client socket.
    pub source: SocketAddr,
    /// The bridge's own socket address, which the player's packets are sent to.
    pub local: SocketAddr,
    pub socket: Arc<UdpSocket>,
    pub cert: DtlsCert,
    /// The server's signaling address, its `server-port`.
    pub server: SocketAddr,
    pub signaling: Arc<Signaling>,
    pub identity: Arc<SignalingIdentity>,
    pub first_packet: Vec<u8>,
    pub from_client: mpsc::Receiver<Vec<u8>>,
}

#[derive(Debug)]
pub enum SessionEnd {
    ChannelNeverOpened,
    ClientGone,
    ClientIdle,
    ClientTooSlow,
    ServerGone,
    ServerTooSlow,
    ServerUnreachable(String),
    ServerRefused(SignalingError),
    Rtc(String),
}

pub fn fingerprint_string(cert: &DtlsCert) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, &cert.certificate);
    let hex: Vec<String> = digest.as_ref().iter().map(|b| format!("{b:02X}")).collect();
    format!("sha-256 {}", hex.join(":"))
}

/// STUN usernames as the player sends and expects them, and as str0m knows them.
struct Usernames {
    from_client_wire: String,
    from_client_rtc: String,
    host_password: String,
    to_client_rtc: String,
    to_client_wire: String,
    client_password: String,
}

impl Usernames {
    fn new(admitted: &Admitted, rtc_ufrag: &str) -> Self {
        let token = &admitted.local_ufrag;
        let client = &admitted.remote_ufrag;
        Usernames {
            from_client_wire: format!("{token}:{client}"),
            from_client_rtc: format!("{rtc_ufrag}:{client}"),
            host_password: admitted.local_password.clone(),
            to_client_rtc: format!("{client}:{rtc_ufrag}"),
            to_client_wire: format!("{client}:{token}"),
            client_password: admitted.remote_password.clone(),
        }
    }
}

fn short_ufrag() -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    (0..16)
        .map(|_| CHARS[rand::random_range(0..CHARS.len())] as char)
        .collect()
}

/// One WebRTC peer and the reliable messages waiting to be written to it.
struct Peer {
    rtc: Rtc,
    reliable: Option<ChannelId>,
    unreliable: Option<ChannelId>,
    queue: VecDeque<Vec<u8>>,
    queued: usize,
}

impl Peer {
    fn new(rtc: Rtc) -> Self {
        Peer {
            rtc,
            reliable: None,
            unreliable: None,
            queue: VecDeque::new(),
            queued: 0,
        }
    }

    /// Queues a reliable message. False when the peer is too far behind.
    fn queue(&mut self, message: Vec<u8>) -> bool {
        self.queued += message.len();
        self.queue.push_back(message);
        self.queued <= MAX_PENDING
    }

    /// Writes queued messages into the reliable channel, as much as SCTP will take.
    fn flush(&mut self) -> Result<(), String> {
        let Some(id) = self.reliable else {
            return Ok(());
        };
        let Some(mut channel) = self.rtc.channel(id) else {
            return Ok(());
        };
        while let Some(message) = self.queue.front() {
            match channel.write(true, message) {
                Ok(true) => {
                    self.queued -= message.len();
                    self.queue.pop_front();
                }
                Ok(false) => break,
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    /// Unreliable messages are dropped rather than queued.
    fn send_unreliable(&mut self, message: &[u8]) {
        if let Some(mut channel) = self.unreliable.and_then(|id| self.rtc.channel(id)) {
            let _ = channel.write(true, message);
        }
    }

    fn is_open(&self) -> bool {
        self.reliable.is_some()
    }
}

/// What one peer's event means for the session.
enum Relay {
    Nothing,
    Reliable(Vec<u8>),
    Unreliable(Vec<u8>),
    Opened,
    Closed,
}

fn on_event(peer: &mut Peer, event: Event) -> Relay {
    match event {
        Event::IceConnectionStateChange(IceConnectionState::Disconnected) => Relay::Closed,
        Event::ChannelOpen(id, label) => {
            if label == RELIABLE_CHANNEL && peer.reliable.is_none() {
                peer.reliable = Some(id);
                Relay::Opened
            } else if label == UNRELIABLE_CHANNEL && peer.unreliable.is_none() {
                peer.unreliable = Some(id);
                Relay::Nothing
            } else {
                tracing::debug!(%label, "ignoring unexpected NetherNet data channel");
                Relay::Nothing
            }
        }
        Event::ChannelData(data) if Some(data.id) == peer.reliable => Relay::Reliable(data.data),
        Event::ChannelData(data) if Some(data.id) == peer.unreliable => {
            Relay::Unreliable(data.data)
        }
        Event::ChannelClose(id) if Some(id) == peer.reliable => Relay::Closed,
        Event::ChannelBufferedAmountLow(_) => Relay::Opened,
        _ => Relay::Nothing,
    }
}

struct Session {
    client: Peer,
    usernames: Usernames,
    source: SocketAddr,
    local: SocketAddr,
    socket: Arc<UdpSocket>,

    server: Peer,
    server_socket: UdpSocket,
    server_local: SocketAddr,

    created: Instant,
    last_from_client: Instant,
}

pub async fn run(setup: SessionSetup) -> SessionEnd {
    let SessionSetup {
        admitted,
        source,
        local,
        socket,
        cert,
        server,
        signaling,
        identity,
        first_packet,
        mut from_client,
    } = setup;

    /* Set up the server's side first; the player's packets wait in the channel meanwhile. */
    let (server_rtc, server_socket, server_local) =
        match connect_server(&signaling, &identity, cert.clone(), server).await {
            Ok(connected) => connected,
            Err(end) => return end,
        };

    let now = Instant::now();
    let rtc_ufrag = short_ufrag();
    let client_rtc = match client_rtc(&admitted, &rtc_ufrag, local, source, cert, now) {
        Ok(rtc) => rtc,
        Err(error) => return SessionEnd::Rtc(error),
    };

    let mut session = Session {
        client: Peer::new(client_rtc),
        usernames: Usernames::new(&admitted, &rtc_ufrag),
        source,
        local,
        socket,
        server: Peer::new(server_rtc),
        server_socket,
        server_local,
        created: now,
        last_from_client: now,
    };

    let end = 'session: {
        if let Err(end) = session.receive_from_client(&first_packet) {
            break 'session end;
        }

        let mut buf = vec![0u8; 2048];
        loop {
            let timeout = match session.drive().await {
                Ok(timeout) => timeout,
                Err(end) => break 'session end,
            };

            let now = Instant::now();
            if !(session.client.is_open() && session.server.is_open())
                && OPEN_TIMEOUT < now.duration_since(session.created)
            {
                break 'session SessionEnd::ChannelNeverOpened;
            }
            if IDLE_TIMEOUT < now.duration_since(session.last_from_client) {
                break 'session SessionEnd::ClientIdle;
            }

            let sleep_until = timeout.min(now + Duration::from_secs(1));
            tokio::select! {
                packet = from_client.recv() => {
                    let Some(packet) = packet else {
                        break 'session SessionEnd::ClientGone;
                    };
                    if let Err(end) = session.receive_from_client(&packet) {
                        break 'session end;
                    }
                }
                received = session.server_socket.recv_from(&mut buf) => match received {
                    Ok((len, from)) => {
                        if let Err(end) = session.receive_from_server(&buf[..len], from) {
                            break 'session end;
                        }
                    }
                    /* Windows reports a closed peer port as a receive error. */
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                    Err(error) => break 'session SessionEnd::ServerUnreachable(error.to_string()),
                },
                _ = tokio::time::sleep_until(sleep_until.into()) => {
                    let now = Instant::now();
                    for rtc in [&mut session.client.rtc, &mut session.server.rtc] {
                        if let Err(error) = rtc.handle_input(Input::Timeout(now)) {
                            break 'session SessionEnd::Rtc(error.to_string());
                        }
                    }
                }
            }
        }
    };

    session.client.rtc.disconnect();
    session.server.rtc.disconnect();
    let _ = session.drive().await;
    end
}

/// The player's side. They already offered through the signaling service; the admission token
/// carries everything their offer said.
fn client_rtc(
    admitted: &Admitted,
    rtc_ufrag: &str,
    local: SocketAddr,
    source: SocketAddr,
    cert: DtlsCert,
    now: Instant,
) -> Result<Rtc, String> {
    let mut rtc = RtcConfig::new()
        .set_local_ice_credentials(IceCreds {
            ufrag: rtc_ufrag.to_string(),
            pass: admitted.local_password.clone(),
        })
        .set_dtls_cert(cert)
        .set_sctp_max_buffered_amount(SCTP_BUFFER)
        .build(now);

    let candidate =
        Candidate::host(local, "udp").map_err(|error| format!("local candidate: {error}"))?;
    rtc.add_local_candidate(candidate);
    if let Ok(candidate) = Candidate::host(source, "udp") {
        rtc.add_remote_candidate(candidate);
    }
    {
        let mut api = rtc.direct_api();
        api.set_remote_ice_credentials(IceCreds {
            ufrag: admitted.remote_ufrag.clone(),
            pass: admitted.remote_password.clone(),
        });
        /* The player made the offer, so it controls ICE. The answer names the host DTLS client. */
        api.set_ice_controlling(false);
        api.set_remote_fingerprint(Fingerprint {
            hash_func: "sha-256".to_string(),
            bytes: admitted.client_fingerprint.to_vec(),
        });
        api.start_dtls(true)
            .map_err(|error| format!("start dtls: {error}"))?;
        /* The client's libwebrtc always opens the SCTP association. Opening it from both ends
         * as well collides, and str0m only settles that on its 3 second INIT retransmit. */
        api.start_sctp(false);
    }
    Ok(rtc)
}

/// The server's side: the bridge plays the client, offering both NetherNet data channels.
async fn connect_server(
    signaling: &Signaling,
    identity: &SignalingIdentity,
    cert: DtlsCert,
    server: SocketAddr,
) -> Result<(Rtc, UdpSocket, SocketAddr), SessionEnd> {
    let unreachable = |error: std::io::Error| SessionEnd::ServerUnreachable(error.to_string());

    /* Bind to the interface that routes to the server, so the offer's candidate is one it can reach. */
    let any = match server {
        SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    let probe = UdpSocket::bind(SocketAddr::new(any, 0))
        .await
        .map_err(unreachable)?;
    probe.connect(server).await.map_err(unreachable)?;
    let route_ip = probe.local_addr().map_err(unreachable)?.ip();
    drop(probe);
    let socket = UdpSocket::bind(SocketAddr::new(route_ip, 0))
        .await
        .map_err(unreachable)?;
    let local = socket.local_addr().map_err(unreachable)?;

    let mut rtc = RtcConfig::new()
        .set_dtls_cert(cert)
        .set_sctp_max_buffered_amount(SCTP_BUFFER)
        .build(Instant::now());
    let candidate = Candidate::host(local, "udp")
        .map_err(|error| SessionEnd::Rtc(format!("local candidate: {error}")))?;
    rtc.add_local_candidate(candidate);

    let mut changes = rtc.sdp_api();
    changes.add_channel_with_config(ChannelConfig {
        label: RELIABLE_CHANNEL.into(),
        ordered: true,
        reliability: Reliability::Reliable,
        negotiated: None,
        protocol: String::new(),
    });
    changes.add_channel_with_config(ChannelConfig {
        label: UNRELIABLE_CHANNEL.into(),
        ordered: false,
        reliability: Reliability::MaxRetransmits { retransmits: 0 },
        negotiated: None,
        protocol: String::new(),
    });
    let Some((offer, pending)) = changes.apply() else {
        return Err(SessionEnd::Rtc("no offer to make".into()));
    };

    let offer = identity.sign_offer(&offer.to_sdp_string(), now_sec() as u64);
    let answer = signaling
        .offer(server, &offer)
        .await
        .map_err(SessionEnd::ServerRefused)?;
    let (answer, port) = prepare_answer(&answer).map_err(SessionEnd::ServerRefused)?;
    let answer = SdpAnswer::from_sdp_string(&answer)
        .map_err(|error| SessionEnd::Rtc(format!("server answer: {error}")))?;
    rtc.sdp_api()
        .accept_answer(pending, answer)
        .map_err(|error| SessionEnd::Rtc(format!("server answer: {error}")))?;

    let remote = SocketAddr::new(server.ip(), port);
    if let Ok(candidate) = Candidate::host(remote, "udp") {
        rtc.add_remote_candidate(candidate);
    }
    Ok((rtc, socket, local))
}

impl Session {
    fn receive_from_client(&mut self, packet: &[u8]) -> Result<(), SessionEnd> {
        self.last_from_client = Instant::now();
        let names = &self.usernames;
        let rewritten = rewrite_username(
            packet,
            &names.from_client_wire,
            &names.from_client_rtc,
            &names.host_password,
        );
        let packet = rewritten.as_deref().unwrap_or(packet);
        let Ok(receive) = Receive::new(Protocol::Udp, self.source, self.local, packet) else {
            return Ok(());
        };
        self.client
            .rtc
            .handle_input(Input::Receive(Instant::now(), receive))
            .map_err(|error| SessionEnd::Rtc(error.to_string()))
    }

    fn receive_from_server(&mut self, packet: &[u8], from: SocketAddr) -> Result<(), SessionEnd> {
        let Ok(receive) = Receive::new(Protocol::Udp, from, self.server_local, packet) else {
            return Ok(());
        };
        self.server
            .rtc
            .handle_input(Input::Receive(Instant::now(), receive))
            .map_err(|error| SessionEnd::Rtc(error.to_string()))
    }

    /// Sends everything either peer has to send and relays their messages. Returns when str0m
    /// next needs a timeout.
    async fn drive(&mut self) -> Result<Instant, SessionEnd> {
        let rtc_error = |error: str0m::RtcError| SessionEnd::Rtc(error.to_string());
        loop {
            self.client.flush().map_err(SessionEnd::Rtc)?;
            self.server.flush().map_err(SessionEnd::Rtc)?;

            let client_timeout = loop {
                match self.client.rtc.poll_output().map_err(rtc_error)? {
                    Output::Timeout(at) => break at,
                    Output::Transmit(transmit) => {
                        let names = &self.usernames;
                        let rewritten = rewrite_username(
                            &transmit.contents,
                            &names.to_client_rtc,
                            &names.to_client_wire,
                            &names.client_password,
                        );
                        let contents = rewritten.as_deref().unwrap_or(&transmit.contents);
                        let _ = self.socket.send_to(contents, transmit.destination).await;
                    }
                    Output::Event(event) => match on_event(&mut self.client, event) {
                        Relay::Nothing => {}
                        Relay::Reliable(message) => {
                            if !self.server.queue(message) {
                                return Err(SessionEnd::ServerTooSlow);
                            }
                            self.server.flush().map_err(SessionEnd::Rtc)?;
                        }
                        Relay::Unreliable(message) => self.server.send_unreliable(&message),
                        Relay::Opened => self.client.flush().map_err(SessionEnd::Rtc)?,
                        Relay::Closed => return Err(SessionEnd::ClientGone),
                    },
                }
            };

            /* Anything the server sends goes out to the player on another pass. */
            let mut for_client = false;
            let server_timeout = loop {
                match self.server.rtc.poll_output().map_err(rtc_error)? {
                    Output::Timeout(at) => break at,
                    Output::Transmit(transmit) => {
                        let _ = self
                            .server_socket
                            .send_to(&transmit.contents, transmit.destination)
                            .await;
                    }
                    Output::Event(event) => match on_event(&mut self.server, event) {
                        Relay::Nothing => {}
                        Relay::Reliable(message) => {
                            if !self.client.queue(message) {
                                return Err(SessionEnd::ClientTooSlow);
                            }
                            for_client = true;
                        }
                        Relay::Unreliable(message) => {
                            self.client.send_unreliable(&message);
                            for_client = true;
                        }
                        Relay::Opened => self.server.flush().map_err(SessionEnd::Rtc)?,
                        Relay::Closed => return Err(SessionEnd::ServerGone),
                    },
                }
            };

            if !for_client {
                return Ok(client_timeout.min(server_timeout));
            }
        }
    }
}
