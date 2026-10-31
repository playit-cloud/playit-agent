//! One player: their NetherNet (WebRTC data channel) connection, joined to a RakNet
//! connection to the Bedrock server.
//!
//! Game packets are copied between the two without being decoded. NetherNet carries Bedrock
//! batches as they are; RakNet prefixes each with 0xFE, which [`RakNetConnection`] adds and
//! removes. The player's login, including the Xbox-signed identity chain, and the encryption
//! handshake that follows reach the server untouched.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::channel::ChannelId;
use str0m::config::DtlsCert;
use str0m::crypto::Fingerprint;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, IceCreds, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::admission::Admitted;
use super::raknet::{CloseReason, RakNetConnection};
use super::stun::rewrite_username;

pub const RELIABLE_CHANNEL: &str = "ReliableDataChannel";
pub const UNRELIABLE_CHANNEL: &str = "UnreliableDataChannel";

/// Payload bytes per SCTP message, the size retail clients use.
const SEGMENT_PAYLOAD: usize = 9999;
/// The segment countdown is one byte.
const MAX_SEGMENTS: usize = 256;
const MAX_ASSEMBLED: usize = 8 * 1024 * 1024;

/// Bytes queued toward the player beyond what SCTP accepted. Past this the player is not
/// keeping up, and the session ends.
const MAX_PENDING_TO_CLIENT: usize = 64 * 1024 * 1024;
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
    pub server: SocketAddr,
    pub first_packet: Vec<u8>,
    pub from_client: mpsc::Receiver<Vec<u8>>,
}

#[derive(Debug)]
pub enum SessionEnd {
    ChannelNeverOpened,
    ClientGone,
    ClientIdle,
    ClientProtocol(&'static str),
    ClientTooSlow,
    Server(CloseReason),
    ServerUnreachable(String),
    Rtc(String),
}

/// Rebuilds messages from NetherNet's `[remaining segments][payload]` framing.
#[derive(Default)]
pub struct SegmentAssembler {
    buffer: Vec<u8>,
    expected: Option<u8>,
}

impl SegmentAssembler {
    pub fn push(&mut self, data: &[u8]) -> Result<Option<Vec<u8>>, &'static str> {
        let (&remaining, payload) = data.split_first().ok_or("empty segment")?;
        if payload.is_empty() {
            return Err("segment without payload");
        }
        if let Some(expected) = self.expected {
            if remaining != expected {
                return Err("segment out of sequence");
            }
        }
        if MAX_ASSEMBLED < self.buffer.len() + payload.len() {
            return Err("message too large");
        }
        self.buffer.extend_from_slice(payload);
        if remaining == 0 {
            self.expected = None;
            return Ok(Some(std::mem::take(&mut self.buffer)));
        }
        self.expected = Some(remaining - 1);
        Ok(None)
    }
}

/// Splits a message into NetherNet segments.
pub fn segment(message: &[u8]) -> Option<Vec<Vec<u8>>> {
    let chunks: Vec<&[u8]> = message.chunks(SEGMENT_PAYLOAD).collect();
    if chunks.is_empty() || MAX_SEGMENTS < chunks.len() {
        return None;
    }
    let last = chunks.len() - 1;
    Some(
        chunks
            .into_iter()
            .enumerate()
            .map(|(index, chunk)| {
                let mut segment = Vec::with_capacity(chunk.len() + 1);
                segment.push((last - index) as u8);
                segment.extend_from_slice(chunk);
                segment
            })
            .collect(),
    )
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
    (0..16).map(|_| CHARS[rand::random_range(0..CHARS.len())] as char).collect()
}

struct Session {
    rtc: Rtc,
    usernames: Usernames,
    source: SocketAddr,
    local: SocketAddr,
    socket: Arc<UdpSocket>,

    reliable: Option<ChannelId>,
    assembler: SegmentAssembler,
    to_client: VecDeque<Vec<u8>>,
    to_client_bytes: usize,
    to_server: Option<mpsc::Sender<Vec<u8>>>,
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
        first_packet,
        mut from_client,
    } = setup;

    let now = Instant::now();
    let rtc_ufrag = short_ufrag();
    let usernames = Usernames::new(&admitted, &rtc_ufrag);
    let mut rtc = RtcConfig::new()
        .set_local_ice_credentials(IceCreds {
            ufrag: rtc_ufrag,
            pass: admitted.local_password.clone(),
        })
        .set_dtls_cert(cert)
        .set_sctp_max_buffered_amount(SCTP_BUFFER)
        .build(now);

    match Candidate::host(local, "udp") {
        Ok(candidate) => {
            rtc.add_local_candidate(candidate);
        }
        Err(error) => return SessionEnd::Rtc(format!("local candidate: {error}")),
    }
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
        if let Err(error) = api.start_dtls(true) {
            return SessionEnd::Rtc(format!("start dtls: {error}"));
        }
        /* The client's libwebrtc always opens the SCTP association. Opening it from both ends
         * as well collides, and str0m only settles that on its 3 second INIT retransmit. */
        api.start_sctp(false);
    }

    /* Connect to the server while WebRTC sets up; packets wait in the channel meanwhile. */
    let (to_server_tx, to_server_rx) = mpsc::channel::<Vec<u8>>(4096);
    let (from_server_tx, mut from_server_rx) = mpsc::channel::<Vec<u8>>(16 * 1024);
    let mut raknet = tokio::spawn(async move {
        match RakNetConnection::connect(server).await {
            Ok(connection) => Ok(connection.run(to_server_rx, from_server_tx).await),
            Err(error) => Err(error.to_string()),
        }
    });

    let mut session = Session {
        rtc,
        usernames,
        source,
        local,
        socket,
        reliable: None,
        assembler: SegmentAssembler::default(),
        to_client: VecDeque::new(),
        to_client_bytes: 0,
        to_server: Some(to_server_tx),
        created: now,
        last_from_client: now,
    };

    let end = 'session: {
        if let Err(end) = session.receive(&first_packet) {
            break 'session end;
        }

        loop {
            let timeout = match session.drive().await {
                Ok(timeout) => timeout,
                Err(end) => break 'session end,
            };

            let now = Instant::now();
            if session.reliable.is_none() && OPEN_TIMEOUT < now.duration_since(session.created) {
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
                    if let Err(end) = session.receive(&packet) {
                        break 'session end;
                    }
                }
                packet = from_server_rx.recv() => {
                    match packet {
                        Some(packet) => {
                            if let Err(end) = session.queue_to_client(&packet) {
                                break 'session end;
                            }
                        }
                        None => {
                            /* The RakNet task ended; its result explains why. */
                            break 'session match (&mut raknet).await {
                                Ok(Ok(reason)) => SessionEnd::Server(reason),
                                Ok(Err(error)) => SessionEnd::ServerUnreachable(error),
                                Err(error) => SessionEnd::ServerUnreachable(error.to_string()),
                            };
                        }
                    }
                }
                _ = tokio::time::sleep_until(sleep_until.into()) => {
                    if let Err(error) = session.rtc.handle_input(Input::Timeout(Instant::now())) {
                        break 'session SessionEnd::Rtc(error.to_string());
                    }
                }
            }
        }
    };

    /* Dropping the sender tells RakNet to disconnect from the server. */
    session.to_server = None;
    session.rtc.disconnect();
    let _ = session.drive().await;
    if tokio::time::timeout(Duration::from_secs(3), &mut raknet).await.is_err() {
        raknet.abort();
    }
    end
}

impl Session {
    fn receive(&mut self, packet: &[u8]) -> Result<(), SessionEnd> {
        self.last_from_client = Instant::now();
        let names = &self.usernames;
        let rewritten = rewrite_username(packet, &names.from_client_wire, &names.from_client_rtc, &names.host_password);
        let packet = rewritten.as_deref().unwrap_or(packet);
        let Ok(receive) = Receive::new(Protocol::Udp, self.source, self.local, packet) else {
            return Ok(());
        };
        self.rtc
            .handle_input(Input::Receive(Instant::now(), receive))
            .map_err(|error| SessionEnd::Rtc(error.to_string()))
    }

    fn queue_to_client(&mut self, message: &[u8]) -> Result<(), SessionEnd> {
        let segments = segment(message).ok_or(SessionEnd::ClientProtocol("server packet too large for NetherNet"))?;
        for segment in segments {
            self.to_client_bytes += segment.len();
            self.to_client.push_back(segment);
        }
        if MAX_PENDING_TO_CLIENT < self.to_client_bytes {
            return Err(SessionEnd::ClientTooSlow);
        }
        Ok(())
    }

    /// Writes queued segments into the data channel, as much as SCTP will take.
    fn flush_to_client(&mut self) -> Result<(), SessionEnd> {
        let Some(id) = self.reliable else {
            return Ok(());
        };
        let Some(mut channel) = self.rtc.channel(id) else {
            return Err(SessionEnd::ClientGone);
        };
        while let Some(segment) = self.to_client.front() {
            match channel.write(true, segment) {
                Ok(true) => {
                    self.to_client_bytes -= segment.len();
                    self.to_client.pop_front();
                }
                Ok(false) => break,
                Err(error) => return Err(SessionEnd::Rtc(error.to_string())),
            }
        }
        Ok(())
    }

    /// Sends everything str0m has to send and handles its events. Returns when str0m next
    /// needs a timeout.
    async fn drive(&mut self) -> Result<Instant, SessionEnd> {
        self.flush_to_client()?;
        loop {
            let output = self
                .rtc
                .poll_output()
                .map_err(|error| SessionEnd::Rtc(error.to_string()))?;
            match output {
                Output::Timeout(at) => return Ok(at),
                Output::Transmit(transmit) => {
                    let names = &self.usernames;
                    let rewritten = rewrite_username(&transmit.contents, &names.to_client_rtc, &names.to_client_wire, &names.client_password);
                    let contents = rewritten.as_deref().unwrap_or(&transmit.contents);
                    let _ = self.socket.send_to(contents, transmit.destination).await;
                }
                Output::Event(event) => self.handle_event(event)?,
            }
        }
    }

    fn handle_event(&mut self, event: Event) -> Result<(), SessionEnd> {
        match event {
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                return Err(SessionEnd::ClientGone);
            }
            Event::ChannelOpen(id, label) => {
                if label == RELIABLE_CHANNEL && self.reliable.is_none() {
                    self.reliable = Some(id);
                    self.flush_to_client()?;
                } else if label != UNRELIABLE_CHANNEL {
                    tracing::debug!(%label, "ignoring unexpected NetherNet data channel");
                }
            }
            Event::ChannelData(data) => {
                if Some(data.id) != self.reliable {
                    /* Game traffic travels on the reliable channel. */
                    return Ok(());
                }
                let message = self
                    .assembler
                    .push(&data.data)
                    .map_err(SessionEnd::ClientProtocol)?;
                if let (Some(message), Some(to_server)) = (message, &self.to_server) {
                    if to_server.try_send(message).is_err() {
                        return Err(SessionEnd::Server(CloseReason::LocalClosed));
                    }
                }
            }
            Event::ChannelClose(id) if Some(id) == self.reliable => {
                return Err(SessionEnd::ClientGone);
            }
            Event::ChannelBufferedAmountLow(_) => self.flush_to_client()?,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_round_trip() {
        let message: Vec<u8> = (0..25_000u32).map(|i| i as u8).collect();
        let segments = segment(&message).unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0][0], 2);
        assert_eq!(segments[2][0], 0);

        let mut assembler = SegmentAssembler::default();
        assert_eq!(assembler.push(&segments[0]), Ok(None));
        assert_eq!(assembler.push(&segments[1]), Ok(None));
        assert_eq!(assembler.push(&segments[2]), Ok(Some(message)));
    }

    #[test]
    fn protocol_vectors() {
        /* From Cloudburst's NetherNet frame vectors. */
        let mut assembler = SegmentAssembler::default();
        assert_eq!(assembler.push(&[0, 1, 2, 3, 4]), Ok(Some(vec![1, 2, 3, 4])));
        assert_eq!(assembler.push(&[2, 0x0a, 0x0b]), Ok(None));
        assert_eq!(assembler.push(&[0, 0x0c]), Err("segment out of sequence"));
        assert_eq!(segment(&[1, 2, 3, 4]).unwrap(), vec![vec![0, 1, 2, 3, 4]]);
    }
}
