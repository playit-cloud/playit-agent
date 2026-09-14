use std::{io, net::SocketAddr, time::Duration};

use message_encoding::MessageEncoding;
use playit_agent_proto::{
    AgentSessionId,
    control_feed::ControlFeed,
    control_messages::{AgentRegistered, ControlRequest, ControlResponse, Ping, Pong},
    raw_slice::RawSlice,
    rpc::ControlRpcMessage,
};
use tokio::time::{Instant, timeout_at};

use super::{api::ControlApi, socket::DualStackUdpSocket};
use crate::{error::SetupError, util::now_milli};

const RECV_BUFFER_LEN: usize = 2048;
const PROBE_REQUEST_ID: u64 = 1;
const PROBE_RESPONSE_WAIT: Duration = Duration::from_secs(1);
const PROBE_ATTEMPTS_IP4: u32 = 3;
const PROBE_ATTEMPTS_IP6: u32 = 1;
const REGISTER_ATTEMPTS: u32 = 5;
const REGISTER_RESPONSE_WAIT: Duration = Duration::from_millis(2_500);
const REGISTER_QUEUED_WAIT: Duration = Duration::from_secs(1);

/// A datagram conversation with one control server.
///
/// The link remembers the most recent pong so callers always know which client and
/// tunnel addresses the server currently observes; registration is signed for those.
pub struct ControlLink {
    socket: DualStackUdpSocket,
    remote: SocketAddr,
    pong: Pong,
    next_request_id: u64,
    send_buffer: Vec<u8>,
    recv_buffer: Vec<u8>,
}

/// A registration the tunnel server accepted.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: AgentSessionId,
    /// Local estimate of when the server forgets the session without keep-alives.
    pub expires_at: Instant,
    /// Addresses the registration was signed for. If the server later reports different
    /// ones the session is no longer usable.
    pub client_addr: SocketAddr,
    pub tunnel_addr: SocketAddr,
    /// Wall clock (ms) when registration completed. Pongs answering pings sent before
    /// this say nothing about the session.
    pub registered_at: u64,
}

impl Session {
    fn new(registered: AgentRegistered, pong: &Pong) -> Self {
        Session {
            id: registered.id,
            expires_at: local_expiry(registered.expires_at, pong.server_now, 0),
            client_addr: pong.client_addr,
            tunnel_addr: pong.tunnel_addr,
            registered_at: now_milli(),
        }
    }

    pub(super) fn apply_registered(&mut self, registered: &AgentRegistered, pong: &Pong) {
        self.id = registered.id.clone();
        self.expires_at = local_expiry(registered.expires_at, pong.server_now, 0);
    }
}

/// Converts a server-clock expiry into a local instant. Only the remaining duration is
/// used so a wrong local clock cannot skew keep-alive scheduling.
pub(super) fn local_expiry(server_expires_at: u64, server_now: u64, rtt_ms: u64) -> Instant {
    let remaining = server_expires_at
        .saturating_sub(server_now)
        .saturating_sub(rtt_ms);
    Instant::now() + Duration::from_millis(remaining)
}

impl ControlLink {
    /// Pings each address in order and returns a link to the first one that answers.
    pub async fn probe(
        socket: DualStackUdpSocket,
        addresses: &[SocketAddr],
    ) -> Result<Self, SetupError> {
        let mut send_buffer = Vec::with_capacity(64);
        let mut recv_buffer = vec![0u8; RECV_BUFFER_LEN];

        for &addr in addresses {
            tracing::debug!(%addr, "probing control address");

            if let Some(pong) =
                probe_address(&socket, addr, &mut send_buffer, &mut recv_buffer).await
            {
                tracing::debug!(%addr, ?pong, "control address responded");
                return Ok(ControlLink {
                    socket,
                    remote: addr,
                    pong,
                    next_request_id: PROBE_REQUEST_ID + 1,
                    send_buffer,
                    recv_buffer,
                });
            }
        }

        Err(SetupError::NoControlResponse)
    }

    pub fn remote(&self) -> SocketAddr {
        self.remote
    }

    /// The most recent pong from this server.
    pub fn pong(&self) -> &Pong {
        &self.pong
    }

    pub fn socket(&self) -> &DualStackUdpSocket {
        &self.socket
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    /// Sends a request and returns the id the server will echo in its response.
    pub async fn send_request(&mut self, request: ControlRequest) -> io::Result<u64> {
        let request_id = self.next_request_id();
        self.send_message(&ControlRpcMessage {
            request_id,
            content: request,
        })
        .await?;
        Ok(request_id)
    }

    async fn send_message<M: MessageEncoding>(
        &mut self,
        message: &ControlRpcMessage<M>,
    ) -> io::Result<()> {
        self.send_buffer.clear();
        message.write_to(&mut self.send_buffer)?;
        self.socket.send_to(&self.send_buffer, self.remote).await?;
        Ok(())
    }

    /// Next decodable message from the control server. Datagrams from other peers and
    /// undecodable datagrams are skipped.
    pub async fn recv(&mut self) -> io::Result<ControlFeed> {
        loop {
            let (len, from) = self.socket.recv_from(&mut self.recv_buffer).await?;
            if from != self.remote {
                tracing::debug!(%from, expected = %self.remote, "ignoring datagram from unexpected peer");
                continue;
            }

            let feed = match ControlFeed::read_from(&mut &self.recv_buffer[..len]) {
                Ok(feed) => feed,
                Err(error) => {
                    tracing::warn!(?error, len, "failed to decode control message");
                    continue;
                }
            };

            if let ControlFeed::Response(ControlRpcMessage {
                content: ControlResponse::Pong(pong),
                ..
            }) = &feed
            {
                self.pong = pong.clone();
            }

            return Ok(feed);
        }
    }

    /// Registers the agent for the addresses in the latest pong.
    ///
    /// Any `NewClient` messages that arrive while registering are dropped: they belong
    /// to a session that no longer exists.
    pub async fn register<A: ControlApi>(&mut self, api: &A) -> Result<Session, SetupError> {
        let signed_for = self.pong.clone();
        let key = api
            .sign_register(signed_for.client_addr, signed_for.tunnel_addr)
            .await?;

        for attempt in 1..=REGISTER_ATTEMPTS {
            let request_id = self.next_request_id();
            self.send_message(&ControlRpcMessage {
                request_id,
                content: RawSlice(&key),
            })
            .await?;

            let deadline = Instant::now() + REGISTER_RESPONSE_WAIT;
            loop {
                let feed = match timeout_at(deadline, self.recv()).await {
                    Ok(result) => result?,
                    Err(_) => {
                        tracing::debug!(attempt, "no response to register request");
                        break;
                    }
                };

                if self.pong.client_addr != signed_for.client_addr
                    || self.pong.tunnel_addr != signed_for.tunnel_addr
                {
                    return Err(SetupError::AddressChanged);
                }

                let ControlFeed::Response(response) = feed else {
                    continue;
                };
                if response.request_id != request_id {
                    continue;
                }

                match response.content {
                    ControlResponse::AgentRegistered(registered) => {
                        return Ok(Session::new(registered, &self.pong));
                    }
                    ControlResponse::InvalidSignature => {
                        return Err(SetupError::RegisterInvalidSignature);
                    }
                    ControlResponse::Unauthorized => {
                        self.refresh_pong().await;
                        return Err(SetupError::RegisterUnauthorized);
                    }
                    ControlResponse::RequestQueued | ControlResponse::TryAgainLater => {
                        tracing::debug!(attempt, "register queued by server, retrying shortly");
                        tokio::time::sleep(REGISTER_QUEUED_WAIT).await;
                        break;
                    }
                    other => {
                        tracing::debug!(?other, "unexpected response to register request");
                    }
                }
            }
        }

        Err(SetupError::RegisterNoResponse)
    }

    /// Sends an unauthenticated ping and waits briefly for the pong so `pong()` reflects
    /// the addresses the server currently sees.
    pub async fn refresh_pong(&mut self) {
        let ping = ControlRequest::Ping(Ping {
            now: now_milli(),
            current_ping: None,
            session_id: None,
        });
        if let Err(error) = self.send_request(ping).await {
            tracing::warn!(?error, "failed to send ping");
            return;
        }

        let deadline = Instant::now() + PROBE_RESPONSE_WAIT;
        while let Ok(Ok(feed)) = timeout_at(deadline, self.recv()).await {
            if matches!(
                feed,
                ControlFeed::Response(ControlRpcMessage {
                    content: ControlResponse::Pong(_),
                    ..
                })
            ) {
                return;
            }
        }
    }
}

async fn probe_address(
    socket: &DualStackUdpSocket,
    addr: SocketAddr,
    send_buffer: &mut Vec<u8>,
    recv_buffer: &mut [u8],
) -> Option<Pong> {
    let attempts = if addr.is_ipv6() {
        PROBE_ATTEMPTS_IP6
    } else {
        PROBE_ATTEMPTS_IP4
    };

    for attempt in 1..=attempts {
        send_buffer.clear();
        let ping = ControlRpcMessage {
            request_id: PROBE_REQUEST_ID,
            content: ControlRequest::Ping(Ping {
                now: now_milli(),
                current_ping: None,
                session_id: None,
            }),
        };
        if let Err(error) = ping.write_to(send_buffer) {
            tracing::error!(?error, "failed to encode ping");
            return None;
        }

        if let Err(error) = socket.send_to(send_buffer, addr).await {
            tracing::debug!(?error, %addr, "failed to send probe ping");
            return None;
        }

        let deadline = Instant::now() + PROBE_RESPONSE_WAIT;
        loop {
            let (len, from) = match timeout_at(deadline, socket.recv_from(recv_buffer)).await {
                Ok(Ok(received)) => received,
                Ok(Err(error)) => {
                    tracing::debug!(?error, %addr, attempt, "probe receive failed");
                    break;
                }
                Err(_) => {
                    tracing::debug!(%addr, attempt, "no pong within probe window");
                    break;
                }
            };

            if from != addr {
                continue;
            }

            match ControlFeed::read_from(&mut &recv_buffer[..len]) {
                Ok(ControlFeed::Response(ControlRpcMessage {
                    request_id: PROBE_REQUEST_ID,
                    content: ControlResponse::Pong(pong),
                })) => return Some(pong),
                Ok(other) => tracing::debug!(?other, "ignoring non-pong during probe"),
                Err(error) => tracing::debug!(?error, "undecodable datagram during probe"),
            }
        }
    }

    None
}
