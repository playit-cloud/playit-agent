//! A minimal RakNet client, enough to carry Bedrock game packets to a Bedrock Dedicated Server.
//!
//! Game packets cross the bridge as opaque bytes: the client's login and Minecraft encryption
//! reach the server unchanged, so the server authenticates the player exactly as it would over
//! a direct RakNet connection.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

const MAGIC: [u8; 16] = [
    0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78,
];
const PROTOCOL_VERSION: u8 = 11;

/// IPv4 and UDP headers, which RakNet counts as part of its MTU.
const UDP_OVERHEAD: usize = 28;
/// Datagram header: flags and a 24-bit sequence number.
const DATAGRAM_HEADER: usize = 4;
/// The largest frame header this client writes: flags, length, reliable and order indexes,
/// order channel, and split details.
const MAX_FRAME_HEADER: usize = 1 + 2 + 3 + 3 + 1 + 10;
/// Bedrock clients and servers exchange 20 internal addresses during the handshake.
const INTERNAL_ADDRESSES: usize = 20;

const MTU_CANDIDATES: [u16; 3] = [1400, 1200, 576];
const ATTEMPTS_PER_MTU: usize = 3;
const OFFLINE_RETRY: Duration = Duration::from_millis(500);

const TICK: Duration = Duration::from_millis(10);
const PING_INTERVAL: Duration = Duration::from_secs(2);
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
const RESEND_AFTER: Duration = Duration::from_millis(300);
/// More unacknowledged data than this means the server is gone or unreachable.
const MAX_UNACKED_DATAGRAMS: usize = 16 * 1024;
const MAX_SPLIT_COUNT: u32 = 8 * 1024;
const MAX_CONCURRENT_SPLITS: usize = 64;
const MAX_ORDER_BUFFER: usize = 4 * 1024;

const ID_CONNECTED_PING: u8 = 0x00;
const ID_CONNECTED_PONG: u8 = 0x03;
const ID_OPEN_CONNECTION_REQUEST_1: u8 = 0x05;
const ID_OPEN_CONNECTION_REPLY_1: u8 = 0x06;
const ID_OPEN_CONNECTION_REQUEST_2: u8 = 0x07;
const ID_OPEN_CONNECTION_REPLY_2: u8 = 0x08;
const ID_CONNECTION_REQUEST: u8 = 0x09;
const ID_CONNECTION_REQUEST_ACCEPTED: u8 = 0x10;
const ID_ALREADY_CONNECTED: u8 = 0x12;
const ID_NEW_INCOMING_CONNECTION: u8 = 0x13;
const ID_NO_FREE_INCOMING_CONNECTIONS: u8 = 0x14;
const ID_DISCONNECTION_NOTIFICATION: u8 = 0x15;
const ID_CONNECTION_BANNED: u8 = 0x17;
const ID_INCOMPATIBLE_PROTOCOL_VERSION: u8 = 0x19;
const ID_GAME_PACKET: u8 = 0xfe;

const FLAG_VALID: u8 = 0x80;
const FLAG_ACK: u8 = 0x40;
const FLAG_NACK: u8 = 0x20;
/// Data datagrams carry the "needs B and AS" bit, as RakNet sets it.
const DATA_DATAGRAM_FLAGS: u8 = FLAG_VALID | 0x04;

const U24_MASK: u32 = 0x00ff_ffff;

#[derive(Debug)]
pub enum RakNetError {
    Io(std::io::Error),
    /// No reply to any connection request.
    Unreachable,
    /// The server refused the connection; the byte is the RakNet message id it sent.
    Refused(u8),
    Malformed,
}

impl From<std::io::Error> for RakNetError {
    fn from(error: std::io::Error) -> Self {
        RakNetError::Io(error)
    }
}

impl std::fmt::Display for RakNetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RakNetError::Io(error) => write!(f, "io error: {error}"),
            RakNetError::Unreachable => write!(f, "no reply from server"),
            RakNetError::Refused(id) => write!(f, "server refused connection (message 0x{id:02x})"),
            RakNetError::Malformed => write!(f, "malformed reply"),
        }
    }
}

/// Why a RakNet connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The server sent a disconnect notification.
    ServerDisconnected,
    /// The server stopped answering.
    TimedOut,
    /// The bridge dropped its side of the connection.
    LocalClosed,
    Io,
}

/// A connected RakNet session to a Bedrock server, run by [`RakNetConnection::run`].
pub struct RakNetConnection {
    socket: UdpSocket,
    server: SocketAddr,
    local: SocketAddr,
    mtu: u16,
    guid: u64,
    started: Instant,

    send: SendState,
    recv: RecvState,

    connected: bool,
    last_received: Instant,
    last_ping: Instant,
}

impl RakNetConnection {
    /// Completes the offline handshake (MTU discovery) with the server at `server`.
    pub async fn connect(server: SocketAddr) -> Result<Self, RakNetError> {
        let bind: SocketAddr = match server.ip() {
            IpAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
            IpAddr::V6(_) => "[::]:0".parse().unwrap(),
        };
        let socket = UdpSocket::bind(bind).await?;
        socket.connect(server).await?;
        let local = socket.local_addr()?;
        let guid: u64 = rand::random();

        let mut buf = vec![0u8; 2048];
        let mut reply_1 = None;

        'mtu: for mtu in MTU_CANDIDATES {
            let request = OfflineMessages::open_connection_request_1(mtu);
            for _ in 0..ATTEMPTS_PER_MTU {
                socket.send(&request).await?;
                let deadline = Instant::now() + OFFLINE_RETRY;
                while let Some(len) = recv_until(&socket, &mut buf, deadline).await? {
                    match OfflineMessages::read_reply_1(&buf[..len]) {
                        Ok(Some(reply)) => {
                            reply_1 = Some(reply);
                            break 'mtu;
                        }
                        Ok(None) => continue,
                        Err(error) => return Err(error),
                    }
                }
            }
        }

        let reply_1 = reply_1.ok_or(RakNetError::Unreachable)?;
        let request_2 = OfflineMessages::open_connection_request_2(&reply_1, server, guid);

        for _ in 0..ATTEMPTS_PER_MTU * 2 {
            socket.send(&request_2).await?;
            let deadline = Instant::now() + OFFLINE_RETRY;
            while let Some(len) = recv_until(&socket, &mut buf, deadline).await? {
                let Some(mtu) = OfflineMessages::read_reply_2(&buf[..len])? else {
                    continue;
                };
                let mtu = mtu.min(reply_1.mtu);
                let now = Instant::now();
                return Ok(RakNetConnection {
                    socket,
                    server,
                    local,
                    mtu,
                    guid,
                    started: now,
                    send: SendState::new(mtu),
                    recv: RecvState::default(),
                    connected: false,
                    last_received: now,
                    last_ping: now,
                });
            }
        }

        Err(RakNetError::Unreachable)
    }

    pub fn mtu(&self) -> u16 {
        self.mtu
    }

    fn now_ms(&self) -> i64 {
        self.started.elapsed().as_millis() as i64
    }

    /// Carries game packets until either side closes. `outbound` packets go to the server;
    /// packets from the server go to `inbound`. Both exclude RakNet's 0xFE game packet id.
    pub async fn run(
        mut self,
        mut outbound: mpsc::Receiver<Vec<u8>>,
        inbound: mpsc::Sender<Vec<u8>>,
    ) -> CloseReason {
        let mut request = Vec::with_capacity(18);
        request.push(ID_CONNECTION_REQUEST);
        request.extend_from_slice(&self.guid.to_be_bytes());
        request.extend_from_slice(&self.now_ms().to_be_bytes());
        request.push(0);
        self.send.queue(request, Reliability::ReliableOrdered);

        let mut buf = vec![0u8; 2048];
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut outbound_open = true;

        let reason = loop {
            tokio::select! {
                received = self.socket.recv(&mut buf) => {
                    let len = match received {
                        Ok(len) => len,
                        /* Windows reports ICMP port unreachable as a receive error. */
                        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => continue,
                        Err(_) => break CloseReason::Io,
                    };
                    self.last_received = Instant::now();
                    if let Some(reason) = self.handle_datagram(&buf[..len], &inbound) {
                        break reason;
                    }
                }
                /* Game packets wait until the server has accepted the connection. */
                packet = outbound.recv(), if outbound_open && self.connected => {
                    match packet {
                        Some(packet) => {
                            let mut body = Vec::with_capacity(packet.len() + 1);
                            body.push(ID_GAME_PACKET);
                            body.extend_from_slice(&packet);
                            self.send.queue(body, Reliability::ReliableOrdered);
                        }
                        None => {
                            outbound_open = false;
                            self.send.queue(vec![ID_DISCONNECTION_NOTIFICATION], Reliability::Reliable);
                        }
                    }
                }
                _ = tick.tick() => {}
            }

            let now = Instant::now();
            if IDLE_TIMEOUT < now.duration_since(self.last_received) {
                break CloseReason::TimedOut;
            }
            if self.connected && PING_INTERVAL < now.duration_since(self.last_ping) {
                self.last_ping = now;
                let mut ping = Vec::with_capacity(9);
                ping.push(ID_CONNECTED_PING);
                ping.extend_from_slice(&self.now_ms().to_be_bytes());
                self.send.queue(ping, Reliability::Unreliable);
            }

            if self.flush(now).await.is_err() {
                break CloseReason::Io;
            }
            if MAX_UNACKED_DATAGRAMS < self.send.unacked.len() {
                break CloseReason::TimedOut;
            }
            if !outbound_open && self.send.is_idle() {
                break CloseReason::LocalClosed;
            }
        };

        if reason != CloseReason::ServerDisconnected && reason != CloseReason::LocalClosed {
            self.send.queue(vec![ID_DISCONNECTION_NOTIFICATION], Reliability::Unreliable);
            let _ = self.flush(Instant::now()).await;
        }
        reason
    }

    async fn flush(&mut self, now: Instant) -> std::io::Result<()> {
        for datagram in self.recv.take_acks() {
            self.socket.send(&datagram).await?;
        }
        for datagram in self.send.take_datagrams(now) {
            match self.socket.send(&datagram).await {
                Ok(_) => {}
                /* An unreachable port shows up on the next send on Windows; RakNet retries. */
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn handle_datagram(&mut self, data: &[u8], inbound: &mpsc::Sender<Vec<u8>>) -> Option<CloseReason> {
        let Some(&flags) = data.first() else {
            return None;
        };
        if flags & FLAG_VALID == 0 {
            /* Late offline replies from the handshake. */
            return None;
        }
        if flags & FLAG_ACK != 0 {
            if let Some(records) = AckRecords::read(&data[1..]) {
                self.send.acknowledge(&records);
            }
            return None;
        }
        if flags & FLAG_NACK != 0 {
            if let Some(records) = AckRecords::read(&data[1..]) {
                self.send.negative_acknowledge(&records);
            }
            return None;
        }

        let mut reader = Reader::new(&data[1..]);
        let Some(sequence) = reader.u24_le() else {
            return None;
        };
        if !self.recv.track_datagram(sequence) {
            return None;
        }

        while !reader.is_empty() {
            let Some(frame) = Frame::read(&mut reader) else {
                return None;
            };
            for body in self.recv.accept_frame(frame) {
                if let Some(reason) = self.handle_message(body, inbound) {
                    return Some(reason);
                }
            }
        }
        None
    }

    fn handle_message(&mut self, body: Vec<u8>, inbound: &mpsc::Sender<Vec<u8>>) -> Option<CloseReason> {
        match body.first().copied()? {
            ID_GAME_PACKET => {
                if inbound.try_send(body[1..].to_vec()).is_err() {
                    /* The NetherNet side is gone or hopelessly behind. */
                    return Some(CloseReason::LocalClosed);
                }
            }
            ID_CONNECTED_PING if body.len() >= 9 => {
                let mut pong = Vec::with_capacity(17);
                pong.push(ID_CONNECTED_PONG);
                pong.extend_from_slice(&body[1..9]);
                pong.extend_from_slice(&self.now_ms().to_be_bytes());
                self.send.queue(pong, Reliability::Unreliable);
            }
            ID_CONNECTION_REQUEST_ACCEPTED if !self.connected => {
                self.connected = true;
                let request_time = body
                    .len()
                    .checked_sub(16)
                    .and_then(|at| body.get(at..at + 8))
                    .map(|bytes| i64::from_be_bytes(bytes.try_into().unwrap()))
                    .unwrap_or(0);

                let mut reply = vec![ID_NEW_INCOMING_CONNECTION];
                write_address(&mut reply, self.server);
                for _ in 0..INTERNAL_ADDRESSES {
                    write_address(&mut reply, internal_placeholder(self.local));
                }
                reply.extend_from_slice(&request_time.to_be_bytes());
                reply.extend_from_slice(&self.now_ms().to_be_bytes());
                self.send.queue(reply, Reliability::ReliableOrdered);
            }
            ID_DISCONNECTION_NOTIFICATION => return Some(CloseReason::ServerDisconnected),
            _ => {}
        }
        None
    }
}

/// The fields of a Bedrock server's unconnected pong (its server list entry).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerStatus {
    pub motd: String,
    pub protocol: Option<u32>,
    pub version: String,
    pub players: u64,
    pub max_players: u64,
    pub level: String,
    /// 0 survival, 1 creative, 2 adventure.
    pub game_type: u8,
}

impl ServerStatus {
    /// Parses `MCPE;<motd>;<protocol>;<version>;<players>;<max>;<guid>;<level>;<game mode>;...`.
    fn parse(text: &str) -> Option<Self> {
        let fields: Vec<&str> = text.split(';').collect();
        if fields.first() != Some(&"MCPE") || fields.len() < 6 {
            return None;
        }
        let game_type = match fields.get(8).map(|mode| mode.to_ascii_lowercase()) {
            Some(mode) if mode == "creative" => 1,
            Some(mode) if mode == "adventure" => 2,
            _ => 0,
        };
        Some(ServerStatus {
            motd: fields[1].to_string(),
            protocol: fields[2].parse().ok(),
            version: fields[3].to_string(),
            players: fields[4].parse().unwrap_or(0),
            max_players: fields[5].parse().unwrap_or(0),
            level: fields.get(7).unwrap_or(&"").to_string(),
            game_type,
        })
    }
}

/// Asks the server for its status the way the server list does.
pub async fn query_status(server: SocketAddr, timeout: Duration) -> Option<ServerStatus> {
    const ID_UNCONNECTED_PING: u8 = 0x01;
    const ID_UNCONNECTED_PONG: u8 = 0x1c;

    let bind: SocketAddr = match server.ip() {
        IpAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
        IpAddr::V6(_) => "[::]:0".parse().unwrap(),
    };
    let socket = UdpSocket::bind(bind).await.ok()?;
    socket.connect(server).await.ok()?;

    let mut ping = vec![ID_UNCONNECTED_PING];
    ping.extend_from_slice(&0i64.to_be_bytes());
    ping.extend_from_slice(&MAGIC);
    ping.extend_from_slice(&rand::random::<u64>().to_be_bytes());

    let deadline = Instant::now() + timeout;
    let mut buf = vec![0u8; 2048];
    for _ in 0..3 {
        socket.send(&ping).await.ok()?;
        let attempt_deadline = (Instant::now() + timeout / 3).min(deadline);
        while let Ok(Some(len)) = recv_until(&socket, &mut buf, attempt_deadline).await {
            let mut reader = Reader::new(&buf[..len]);
            if reader.u8() != Some(ID_UNCONNECTED_PONG) {
                continue;
            }
            let _time = reader.u64_be()?;
            let _guid = reader.u64_be()?;
            if reader.bytes(16) != Some(&MAGIC[..]) {
                continue;
            }
            /* With LAN visibility off the server answers without its status text. */
            let text = reader
                .u16_be()
                .and_then(|len| reader.bytes(len as usize))
                .and_then(|text| std::str::from_utf8(text).ok())
                .unwrap_or("");
            return Some(ServerStatus::parse(text).unwrap_or_default());
        }
    }
    None
}

async fn recv_until(
    socket: &UdpSocket,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<Option<usize>, RakNetError> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        match tokio::time::timeout(remaining, socket.recv(buf)).await {
            Err(_) => return Ok(None),
            Ok(Ok(len)) => return Ok(Some(len)),
            /* Nothing listening yet; keep trying until the deadline. */
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                tokio::time::sleep(remaining.min(Duration::from_millis(50))).await;
            }
            Ok(Err(error)) => return Err(error.into()),
        }
    }
}

fn internal_placeholder(local: SocketAddr) -> SocketAddr {
    match local {
        SocketAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
        SocketAddr::V6(_) => "[::]:0".parse().unwrap(),
    }
}

struct ReplyOne {
    use_security: bool,
    cookie: Option<u32>,
    mtu: u16,
}

struct OfflineMessages;

impl OfflineMessages {
    /// Padded so the whole IP packet is `mtu` bytes; the server answers only if it arrives.
    fn open_connection_request_1(mtu: u16) -> Vec<u8> {
        let len = (mtu as usize).saturating_sub(UDP_OVERHEAD).max(18);
        let mut packet = Vec::with_capacity(len);
        packet.push(ID_OPEN_CONNECTION_REQUEST_1);
        packet.extend_from_slice(&MAGIC);
        packet.push(PROTOCOL_VERSION);
        packet.resize(len, 0);
        packet
    }

    fn read_reply_1(data: &[u8]) -> Result<Option<ReplyOne>, RakNetError> {
        Self::check_refusal(data)?;
        let mut reader = Reader::new(data);
        if reader.u8() != Some(ID_OPEN_CONNECTION_REPLY_1) || reader.bytes(16) != Some(&MAGIC[..]) {
            return Ok(None);
        }
        let _server_guid = reader.u64_be().ok_or(RakNetError::Malformed)?;
        let use_security = reader.u8().ok_or(RakNetError::Malformed)? != 0;
        let cookie = if use_security {
            Some(reader.u32_be().ok_or(RakNetError::Malformed)?)
        } else {
            None
        };
        let mtu = reader.u16_be().ok_or(RakNetError::Malformed)?;
        Ok(Some(ReplyOne {
            use_security,
            cookie,
            mtu,
        }))
    }

    fn open_connection_request_2(reply: &ReplyOne, server: SocketAddr, guid: u64) -> Vec<u8> {
        let mut packet = Vec::with_capacity(64);
        packet.push(ID_OPEN_CONNECTION_REQUEST_2);
        packet.extend_from_slice(&MAGIC);
        if let Some(cookie) = reply.cookie {
            packet.extend_from_slice(&cookie.to_be_bytes());
            /* The client does not hold the server's public key. */
            packet.push(0);
        }
        let _ = reply.use_security;
        write_address(&mut packet, server);
        packet.extend_from_slice(&reply.mtu.to_be_bytes());
        packet.extend_from_slice(&guid.to_be_bytes());
        packet
    }

    fn read_reply_2(data: &[u8]) -> Result<Option<u16>, RakNetError> {
        Self::check_refusal(data)?;
        let mut reader = Reader::new(data);
        if reader.u8() != Some(ID_OPEN_CONNECTION_REPLY_2) || reader.bytes(16) != Some(&MAGIC[..]) {
            return Ok(None);
        }
        let _server_guid = reader.u64_be().ok_or(RakNetError::Malformed)?;
        read_address(&mut reader).ok_or(RakNetError::Malformed)?;
        let mtu = reader.u16_be().ok_or(RakNetError::Malformed)?;
        Ok(Some(mtu))
    }

    fn check_refusal(data: &[u8]) -> Result<(), RakNetError> {
        match data.first().copied() {
            Some(
                id @ (ID_ALREADY_CONNECTED
                | ID_NO_FREE_INCOMING_CONNECTIONS
                | ID_CONNECTION_BANNED
                | ID_INCOMPATIBLE_PROTOCOL_VERSION),
            ) if data.get(1..17) == Some(&MAGIC[..]) => Err(RakNetError::Refused(id)),
            _ => Ok(()),
        }
    }
}

fn write_address(out: &mut Vec<u8>, addr: SocketAddr) {
    match addr {
        SocketAddr::V4(addr) => {
            out.push(4);
            out.extend(addr.ip().octets().iter().map(|b| !b));
            out.extend_from_slice(&addr.port().to_be_bytes());
        }
        SocketAddr::V6(addr) => {
            out.push(6);
            /* sockaddr_in6 as Windows lays it out: family 23, little endian. */
            out.extend_from_slice(&23u16.to_le_bytes());
            out.extend_from_slice(&addr.port().to_be_bytes());
            out.extend_from_slice(&addr.flowinfo().to_be_bytes());
            out.extend_from_slice(&addr.ip().octets());
            out.extend_from_slice(&addr.scope_id().to_be_bytes());
        }
    }
}

fn read_address(reader: &mut Reader) -> Option<()> {
    match reader.u8()? {
        4 => reader.bytes(6).map(|_| ()),
        6 => reader.bytes(28).map(|_| ()),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reliability {
    Unreliable,
    UnreliableSequenced,
    Reliable,
    ReliableOrdered,
    ReliableSequenced,
    UnreliableAckReceipt,
    ReliableAckReceipt,
    ReliableOrderedAckReceipt,
}

impl Reliability {
    fn from_bits(bits: u8) -> Self {
        match bits {
            0 => Reliability::Unreliable,
            1 => Reliability::UnreliableSequenced,
            2 => Reliability::Reliable,
            3 => Reliability::ReliableOrdered,
            4 => Reliability::ReliableSequenced,
            5 => Reliability::UnreliableAckReceipt,
            6 => Reliability::ReliableAckReceipt,
            _ => Reliability::ReliableOrderedAckReceipt,
        }
    }

    fn bits(self) -> u8 {
        self as u8
    }

    fn is_reliable(self) -> bool {
        matches!(
            self,
            Reliability::Reliable
                | Reliability::ReliableOrdered
                | Reliability::ReliableSequenced
                | Reliability::ReliableAckReceipt
                | Reliability::ReliableOrderedAckReceipt
        )
    }

    fn is_sequenced(self) -> bool {
        matches!(self, Reliability::UnreliableSequenced | Reliability::ReliableSequenced)
    }

    fn is_ordered(self) -> bool {
        matches!(self, Reliability::ReliableOrdered | Reliability::ReliableOrderedAckReceipt)
    }

    fn has_order_fields(self) -> bool {
        self.is_ordered() || self.is_sequenced()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Split {
    count: u32,
    id: u16,
    index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    reliability: Reliability,
    reliable_index: u32,
    sequence_index: u32,
    order_index: u32,
    order_channel: u8,
    split: Option<Split>,
    body: Vec<u8>,
}

impl Frame {
    fn read(reader: &mut Reader) -> Option<Frame> {
        let flags = reader.u8()?;
        let reliability = Reliability::from_bits(flags >> 5);
        let is_split = flags & 0x10 != 0;
        let bit_len = reader.u16_be()? as usize;
        let len = bit_len.div_ceil(8);

        let reliable_index = if reliability.is_reliable() { reader.u24_le()? } else { 0 };
        let sequence_index = if reliability.is_sequenced() { reader.u24_le()? } else { 0 };
        let (order_index, order_channel) = if reliability.has_order_fields() {
            (reader.u24_le()?, reader.u8()?)
        } else {
            (0, 0)
        };
        let split = if is_split {
            Some(Split {
                count: reader.u32_be()?,
                id: reader.u16_be()?,
                index: reader.u32_be()?,
            })
        } else {
            None
        };
        let body = reader.bytes(len)?.to_vec();

        Some(Frame {
            reliability,
            reliable_index,
            sequence_index,
            order_index,
            order_channel,
            split,
            body,
        })
    }

    fn encoded_len(&self) -> usize {
        let mut len = 3 + self.body.len();
        if self.reliability.is_reliable() {
            len += 3;
        }
        if self.reliability.is_sequenced() {
            len += 3;
        }
        if self.reliability.has_order_fields() {
            len += 4;
        }
        if self.split.is_some() {
            len += 10;
        }
        len
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut flags = self.reliability.bits() << 5;
        if self.split.is_some() {
            flags |= 0x10;
        }
        out.push(flags);
        out.extend_from_slice(&((self.body.len() * 8) as u16).to_be_bytes());
        if self.reliability.is_reliable() {
            write_u24_le(out, self.reliable_index);
        }
        if self.reliability.is_sequenced() {
            write_u24_le(out, self.sequence_index);
        }
        if self.reliability.has_order_fields() {
            write_u24_le(out, self.order_index);
            out.push(self.order_channel);
        }
        if let Some(split) = self.split {
            out.extend_from_slice(&split.count.to_be_bytes());
            out.extend_from_slice(&split.id.to_be_bytes());
            out.extend_from_slice(&split.index.to_be_bytes());
        }
        out.extend_from_slice(&self.body);
    }
}

struct SentDatagram {
    frames: Vec<Frame>,
    sent_at: Instant,
}

struct SendState {
    max_datagram: usize,
    next_sequence: u32,
    next_reliable_index: u32,
    next_order_index: u32,
    next_split_id: u16,

    queue: VecDeque<Frame>,
    unacked: HashMap<u32, SentDatagram>,
}

impl SendState {
    fn new(mtu: u16) -> Self {
        SendState {
            max_datagram: (mtu as usize).saturating_sub(UDP_OVERHEAD).max(400),
            next_sequence: 0,
            next_reliable_index: 0,
            next_order_index: 0,
            next_split_id: 0,
            queue: VecDeque::new(),
            unacked: HashMap::new(),
        }
    }

    fn is_idle(&self) -> bool {
        self.queue.is_empty() && self.unacked.is_empty()
    }

    fn queue(&mut self, body: Vec<u8>, reliability: Reliability) {
        let order_index = if reliability.is_ordered() {
            let index = self.next_order_index;
            self.next_order_index = (index + 1) & U24_MASK;
            index
        } else {
            0
        };

        let max_body = self.max_datagram - DATAGRAM_HEADER - MAX_FRAME_HEADER;
        if body.len() <= max_body {
            let reliable_index = self.take_reliable_index(reliability);
            self.queue.push_back(Frame {
                reliability,
                reliable_index,
                sequence_index: 0,
                order_index,
                order_channel: 0,
                split: None,
                body,
            });
            return;
        }

        /* Only reliable frames may be split; fragments without retransmission would lose the packet. */
        let reliability = match reliability {
            Reliability::Unreliable => Reliability::Reliable,
            other => other,
        };
        let id = self.next_split_id;
        self.next_split_id = self.next_split_id.wrapping_add(1);
        let chunks: Vec<&[u8]> = body.chunks(max_body).collect();
        let count = chunks.len() as u32;
        for (index, chunk) in chunks.into_iter().enumerate() {
            let reliable_index = self.take_reliable_index(reliability);
            self.queue.push_back(Frame {
                reliability,
                reliable_index,
                sequence_index: 0,
                order_index,
                order_channel: 0,
                split: Some(Split {
                    count,
                    id,
                    index: index as u32,
                }),
                body: chunk.to_vec(),
            });
        }
    }

    fn take_reliable_index(&mut self, reliability: Reliability) -> u32 {
        if !reliability.is_reliable() {
            return 0;
        }
        let index = self.next_reliable_index;
        self.next_reliable_index = (index + 1) & U24_MASK;
        index
    }

    fn acknowledge(&mut self, records: &AckRecords) {
        for sequence in records.iter() {
            self.unacked.remove(&sequence);
        }
    }

    fn negative_acknowledge(&mut self, records: &AckRecords) {
        for sequence in records.iter() {
            if let Some(sent) = self.unacked.remove(&sequence) {
                for frame in sent.frames.into_iter().rev() {
                    self.queue.push_front(frame);
                }
            }
        }
    }

    /// Requeues reliable frames that went unacknowledged too long, then packs the queue
    /// into datagrams.
    fn take_datagrams(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let expired: Vec<u32> = self
            .unacked
            .iter()
            .filter(|(_, sent)| RESEND_AFTER < now.duration_since(sent.sent_at))
            .map(|(&sequence, _)| sequence)
            .collect();
        let mut expired = expired;
        expired.sort_unstable_by(|a, b| seq_cmp(*a, *b));
        for sequence in expired.into_iter().rev() {
            let sent = self.unacked.remove(&sequence).unwrap();
            for frame in sent.frames.into_iter().rev() {
                self.queue.push_front(frame);
            }
        }

        let mut datagrams = Vec::new();
        while !self.queue.is_empty() {
            let sequence = self.next_sequence;
            self.next_sequence = (sequence + 1) & U24_MASK;

            let mut datagram = Vec::with_capacity(self.max_datagram);
            datagram.push(DATA_DATAGRAM_FLAGS);
            write_u24_le(&mut datagram, sequence);

            let mut reliable_frames = Vec::new();
            while let Some(frame) = self.queue.front() {
                let fits = datagram.len() + frame.encoded_len() <= self.max_datagram;
                if !fits && datagram.len() > DATAGRAM_HEADER {
                    break;
                }
                let frame = self.queue.pop_front().unwrap();
                frame.write(&mut datagram);
                if frame.reliability.is_reliable() {
                    reliable_frames.push(frame);
                }
            }

            if !reliable_frames.is_empty() {
                self.unacked.insert(
                    sequence,
                    SentDatagram {
                        frames: reliable_frames,
                        sent_at: now,
                    },
                );
            }
            datagrams.push(datagram);
        }
        datagrams
    }
}

struct PendingSplit {
    parts: Vec<Option<Vec<u8>>>,
    received: u32,
}

#[derive(Default)]
struct RecvState {
    /// Datagram sequence numbers to acknowledge on the next flush.
    pending_acks: BTreeSet<u32>,
    pending_nacks: BTreeSet<u32>,
    next_datagram: Option<u32>,
    seen_datagrams: WindowSet,

    seen_reliable: WindowSet,
    splits: HashMap<u16, PendingSplit>,

    next_order: [u32; 32],
    order_buffer: [BTreeMap<u32, Vec<u8>>; 32],
    highest_sequence: [Option<u32>; 32],
}

impl RecvState {
    /// Records a datagram for acknowledgement. Returns false for a duplicate.
    fn track_datagram(&mut self, sequence: u32) -> bool {
        self.pending_acks.insert(sequence);
        self.pending_nacks.remove(&sequence);

        if let Some(next) = self.next_datagram {
            let ahead = sequence.wrapping_sub(next) & U24_MASK;
            if ahead < 0x80_0000 {
                /* Datagrams skipped between `next` and this one are reported missing. */
                for missing in 0..ahead.min(256) {
                    self.pending_nacks.insert((next + missing) & U24_MASK);
                }
                self.next_datagram = Some((sequence + 1) & U24_MASK);
            }
        } else {
            self.next_datagram = Some((sequence + 1) & U24_MASK);
        }

        self.seen_datagrams.insert(sequence)
    }

    fn take_acks(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if !self.pending_acks.is_empty() {
            let acks = std::mem::take(&mut self.pending_acks);
            out.extend(AckRecords::encode(FLAG_VALID | FLAG_ACK, acks.into_iter()));
        }
        if !self.pending_nacks.is_empty() {
            let nacks = std::mem::take(&mut self.pending_nacks);
            out.extend(AckRecords::encode(FLAG_VALID | FLAG_NACK, nacks.into_iter()));
        }
        out
    }

    /// Returns the message bodies this frame completes, in delivery order.
    fn accept_frame(&mut self, frame: Frame) -> Vec<Vec<u8>> {
        if frame.reliability.is_reliable() && !self.seen_reliable.insert(frame.reliable_index) {
            return Vec::new();
        }

        let body = match frame.split {
            None => frame.body,
            Some(split) => match self.reassemble(split, frame.body) {
                Some(body) => body,
                None => return Vec::new(),
            },
        };

        let channel = (frame.order_channel as usize) % 32;
        if frame.reliability.is_sequenced() {
            /* Sequenced frames drop anything older than the newest seen. */
            let highest = &mut self.highest_sequence[channel];
            if let Some(highest) = *highest {
                if seq_cmp(frame.sequence_index, highest) != std::cmp::Ordering::Greater {
                    return Vec::new();
                }
            }
            *highest = Some(frame.sequence_index);
            return vec![body];
        }
        if !frame.reliability.is_ordered() {
            return vec![body];
        }

        let expected = self.next_order[channel];
        match seq_cmp(frame.order_index, expected) {
            std::cmp::Ordering::Less => Vec::new(),
            std::cmp::Ordering::Greater => {
                if self.order_buffer[channel].len() < MAX_ORDER_BUFFER {
                    self.order_buffer[channel].insert(frame.order_index, body);
                }
                Vec::new()
            }
            std::cmp::Ordering::Equal => {
                let mut out = vec![body];
                let mut next = (expected + 1) & U24_MASK;
                while let Some(body) = self.order_buffer[channel].remove(&next) {
                    out.push(body);
                    next = (next + 1) & U24_MASK;
                }
                self.next_order[channel] = next;
                out
            }
        }
    }

    fn reassemble(&mut self, split: Split, body: Vec<u8>) -> Option<Vec<u8>> {
        if split.count == 0 || MAX_SPLIT_COUNT < split.count || split.count <= split.index {
            return None;
        }
        if !self.splits.contains_key(&split.id) && MAX_CONCURRENT_SPLITS <= self.splits.len() {
            return None;
        }
        let pending = self.splits.entry(split.id).or_insert_with(|| PendingSplit {
            parts: vec![None; split.count as usize],
            received: 0,
        });
        if pending.parts.len() != split.count as usize {
            return None;
        }
        let slot = &mut pending.parts[split.index as usize];
        if slot.is_none() {
            *slot = Some(body);
            pending.received += 1;
        }
        if pending.received < split.count {
            return None;
        }
        let pending = self.splits.remove(&split.id)?;
        Some(pending.parts.into_iter().flatten().flatten().collect())
    }
}

/// Remembers 24-bit sequence numbers within a sliding window, to drop duplicates.
#[derive(Default)]
struct WindowSet {
    base: u32,
    above: BTreeSet<u32>,
}

impl WindowSet {
    /// Returns false if `value` was seen before or falls behind the window.
    fn insert(&mut self, value: u32) -> bool {
        let offset = value.wrapping_sub(self.base) & U24_MASK;
        if 0x80_0000 <= offset {
            return false;
        }
        if !self.above.insert(offset) {
            return false;
        }
        while self.above.remove(&0) {
            self.base = (self.base + 1) & U24_MASK;
            self.above = self.above.iter().map(|o| o - 1).collect();
        }
        /* Bound memory if the peer skips ahead and never fills the gap. */
        if 4096 < self.above.len() {
            let shift = *self.above.iter().next().unwrap();
            self.base = (self.base + shift) & U24_MASK;
            self.above = self.above.iter().map(|o| o - shift).collect();
        }
        true
    }
}

/// Compares 24-bit sequence numbers allowing for wraparound.
fn seq_cmp(a: u32, b: u32) -> std::cmp::Ordering {
    if a == b {
        return std::cmp::Ordering::Equal;
    }
    if (a.wrapping_sub(b) & U24_MASK) < 0x80_0000 {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Less
    }
}

struct AckRecords {
    ranges: Vec<(u32, u32)>,
}

impl AckRecords {
    fn read(data: &[u8]) -> Option<AckRecords> {
        let mut reader = Reader::new(data);
        let count = reader.u16_be()?;
        let mut ranges = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let single = reader.u8()? != 0;
            let start = reader.u24_le()?;
            let end = if single { start } else { reader.u24_le()? };
            if 4096 < (end.wrapping_sub(start) & U24_MASK) {
                return None;
            }
            ranges.push((start, end));
        }
        Some(AckRecords { ranges })
    }

    fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.ranges.iter().flat_map(|&(start, end)| {
            let len = end.wrapping_sub(start) & U24_MASK;
            (0..=len).map(move |offset| (start + offset) & U24_MASK)
        })
    }

    /// Encodes sorted sequence numbers as range records, split across datagrams if needed.
    fn encode(flags: u8, sequences: impl Iterator<Item = u32>) -> Vec<Vec<u8>> {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for sequence in sequences {
            match ranges.last_mut() {
                Some((_, end)) if (*end + 1) & U24_MASK == sequence => *end = sequence,
                _ => ranges.push((sequence, sequence)),
            }
        }

        ranges
            .chunks(128)
            .map(|chunk| {
                let mut out = Vec::with_capacity(3 + chunk.len() * 7);
                out.push(flags);
                out.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
                for &(start, end) in chunk {
                    if start == end {
                        out.push(1);
                        write_u24_le(&mut out, start);
                    } else {
                        out.push(0);
                        write_u24_le(&mut out, start);
                        write_u24_le(&mut out, end);
                    }
                }
                out
            })
            .collect()
    }
}

fn write_u24_le(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes()[..3]);
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data }
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.data.len() < len {
            return None;
        }
        let (head, rest) = self.data.split_at(len);
        self.data = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.bytes(1).map(|b| b[0])
    }

    fn u16_be(&mut self) -> Option<u16> {
        self.bytes(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }

    fn u24_le(&mut self) -> Option<u32> {
        self.bytes(3).map(|b| u32::from_le_bytes([b[0], b[1], b[2], 0]))
    }

    fn u32_be(&mut self) -> Option<u32> {
        self.bytes(4).map(|b| u32::from_be_bytes(b.try_into().unwrap()))
    }

    fn u64_be(&mut self) -> Option<u64> {
        self.bytes(8).map(|b| u64::from_be_bytes(b.try_into().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_datagram(data: &[u8]) -> (u32, Vec<Frame>) {
        assert_eq!(data[0], DATA_DATAGRAM_FLAGS);
        let mut reader = Reader::new(&data[1..]);
        let sequence = reader.u24_le().unwrap();
        let mut frames = Vec::new();
        while !reader.is_empty() {
            frames.push(Frame::read(&mut reader).unwrap());
        }
        (sequence, frames)
    }

    #[test]
    fn large_packets_split_and_reassemble_in_order() {
        let mut send = SendState::new(1400);
        let big: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        send.queue(vec![1, 2, 3], Reliability::ReliableOrdered);
        send.queue(big.clone(), Reliability::ReliableOrdered);
        send.queue(vec![4], Reliability::ReliableOrdered);

        let datagrams = send.take_datagrams(Instant::now());
        assert!(datagrams.iter().all(|d| d.len() <= 1400 - UDP_OVERHEAD));

        let mut recv = RecvState::default();
        let mut delivered = Vec::new();
        /* Deliver datagrams in reverse to exercise ordering and reassembly. */
        for datagram in datagrams.iter().rev() {
            let (sequence, frames) = parse_datagram(datagram);
            assert!(recv.track_datagram(sequence));
            for frame in frames {
                delivered.extend(recv.accept_frame(frame));
            }
        }
        assert_eq!(delivered, vec![vec![1, 2, 3], big, vec![4]]);
    }

    #[test]
    fn duplicates_are_dropped_and_nack_resends() {
        let mut send = SendState::new(1400);
        send.queue(vec![9; 10], Reliability::ReliableOrdered);
        let first = send.take_datagrams(Instant::now());
        assert_eq!(first.len(), 1);

        let (sequence, _) = parse_datagram(&first[0]);
        send.negative_acknowledge(&AckRecords {
            ranges: vec![(sequence, sequence)],
        });
        let resent = send.take_datagrams(Instant::now());
        assert_eq!(resent.len(), 1);

        let mut recv = RecvState::default();
        let mut delivered = Vec::new();
        for datagram in first.iter().chain(resent.iter()) {
            let (sequence, frames) = parse_datagram(datagram);
            recv.track_datagram(sequence);
            for frame in frames {
                delivered.extend(recv.accept_frame(frame));
            }
        }
        assert_eq!(delivered, vec![vec![9; 10]]);

        let acks = recv.take_acks();
        let records = AckRecords::read(&acks[0][1..]).unwrap();
        send.acknowledge(&records);
        assert!(send.is_idle());
    }

    #[test]
    fn missing_datagrams_are_nacked() {
        let mut recv = RecvState::default();
        recv.track_datagram(0);
        recv.track_datagram(3);
        let packets = recv.take_acks();
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[1][0], FLAG_VALID | FLAG_NACK);
        let nacks: Vec<u32> = AckRecords::read(&packets[1][1..]).unwrap().iter().collect();
        assert_eq!(nacks, vec![1, 2]);
    }

    #[test]
    fn window_set_wraps() {
        let mut set = WindowSet {
            base: U24_MASK - 1,
            above: BTreeSet::new(),
        };
        assert!(set.insert(U24_MASK - 1));
        assert!(set.insert(U24_MASK));
        assert!(set.insert(0));
        assert!(!set.insert(U24_MASK));
        assert!(!set.insert(0));
        assert_eq!(set.base, 1);
    }

    #[test]
    fn parses_dedicated_server_status() {
        let status = ServerStatus::parse(
            "MCPE;Dedicated Server;844;1.21.111;2;10;13253860892328930865;Bedrock level;Creative;1;19132;19133;0;",
        )
        .unwrap();
        assert_eq!(status.motd, "Dedicated Server");
        assert_eq!(status.protocol, Some(844));
        assert_eq!(status.players, 2);
        assert_eq!(status.max_players, 10);
        assert_eq!(status.level, "Bedrock level");
        assert_eq!(status.game_type, 1);
    }

    #[test]
    fn ipv4_address_encoding() {
        let mut out = Vec::new();
        write_address(&mut out, "127.0.0.1:19132".parse().unwrap());
        assert_eq!(out, vec![4, 0x80, 0xff, 0xff, 0xfe, 0x4a, 0xbc]);
    }
}
