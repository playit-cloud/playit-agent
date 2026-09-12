use std::{collections::VecDeque, net::SocketAddr, time::Duration};

use message_encoding::MessageEncoding;
use playit_agent_proto::{
    control_feed::ControlFeed,
    control_messages::{AgentRegistered, ControlRequest, ControlResponse, Ping, Pong},
    raw_slice::RawSlice,
    rpc::ControlRpcMessage,
};

use crate::utils::now_milli;

use super::{
    AuthResource, PacketIO,
    errors::{ControlError, SetupError},
    established_control::EstablishedControl,
};

#[derive(Debug)]
pub struct ConnectedControl<IO: PacketIO> {
    pub(super) control_addr: SocketAddr,
    pub(super) packet_io: IO,
    pub(super) pong_latest: Pong,
    pub(super) pong_received_at: tokio::time::Instant,
    pub(super) buffer: Vec<u8>,
    pending: VecDeque<ControlFeed>,
}

impl<IO: PacketIO> ConnectedControl<IO> {
    pub fn new(control_addr: SocketAddr, udp: IO, pong: Pong) -> Self {
        ConnectedControl {
            control_addr,
            packet_io: udp,
            pong_latest: pong,
            pong_received_at: tokio::time::Instant::now(),
            buffer: Vec::with_capacity(65_536),
            pending: VecDeque::new(),
        }
    }

    pub fn control_addr(&self) -> SocketAddr {
        self.control_addr
    }

    pub fn pong(&self) -> Pong {
        self.pong_latest.clone()
    }

    pub async fn auth_into_established<A: AuthResource>(
        mut self,
        auth: A,
    ) -> Result<EstablishedControl<A, IO>, SetupError> {
        let registered = self.authenticate(&auth).await?;
        Ok(self.into_established(auth, registered))
    }

    pub fn into_established<A: AuthResource>(
        self,
        auth: A,
        registered: AgentRegistered,
    ) -> EstablishedControl<A, IO> {
        EstablishedControl::new(auth, self, registered)
    }

    pub async fn refresh_pong(&mut self) -> Result<(), SetupError> {
        let now = now_milli();
        let request_id = super::next_request_id();
        self.send(&ControlRpcMessage {
            request_id,
            content: ControlRequest::Ping(Ping {
                now,
                current_ping: None,
                session_id: None,
            }),
        })
        .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let feed = match tokio::time::timeout_at(deadline, self.recv_wire()).await {
                Ok(Ok(feed)) => feed,
                Ok(Err(_)) => continue,
                Err(_) => return Err(SetupError::NoResponseFromAuthenticate),
            };
            match feed {
                ControlFeed::Response(response) if response.request_id == request_id => {
                    if let ControlResponse::Pong(pong) = response.content
                        && pong.request_now == now
                    {
                        self.pong_latest = pong;
                        self.pong_received_at = tokio::time::Instant::now();
                        return Ok(());
                    }
                }
                event @ (ControlFeed::NewClient(_) | ControlFeed::NewClientOld(_)) => {
                    self.queue_event(event)
                }
                _ => {}
            }
        }
    }

    fn queue_event(&mut self, event: ControlFeed) {
        if self.pending.len() < 256 {
            self.pending.push_back(event);
        }
    }

    pub async fn authenticate<A: AuthResource>(
        &mut self,
        auth: &A,
    ) -> Result<AgentRegistered, SetupError> {
        let auth_pong = self.pong_latest.clone();
        let res = auth.authenticate(&auth_pong).await?;

        let bytes = match hex::decode(&res.key) {
            Ok(data) => data,
            Err(_) => return Err(SetupError::FailedToDecodeSignedAgentRegisterHex),
        };

        let request_id = super::next_request_id();

        for _ in 0..5 {
            self.send(&ControlRpcMessage {
                request_id,
                content: RawSlice(&bytes),
            })
            .await?;
            let deadline = tokio::time::Instant::now() + Duration::from_millis(2500);
            loop {
                let feed = match tokio::time::timeout_at(deadline, self.recv_wire()).await {
                    Ok(Ok(feed)) => feed,
                    Ok(Err(ControlError::IoError(error))) => return Err(error.into()),
                    Ok(Err(_)) => continue,
                    Err(_) => break,
                };
                let response = match feed {
                    ControlFeed::Response(response) if response.request_id == request_id => {
                        response
                    }
                    event @ (ControlFeed::NewClient(_) | ControlFeed::NewClientOld(_)) => {
                        self.queue_event(event);
                        continue;
                    }
                    _ => continue,
                };
                match response.content {
                    ControlResponse::AgentRegistered(registered) => return Ok(registered),
                    ControlResponse::InvalidSignature => {
                        return Err(SetupError::RegisterInvalidSignature);
                    }
                    ControlResponse::Unauthorized => return Err(SetupError::RegisterUnauthorized),
                    ControlResponse::Pong(pong)
                        if pong.client_addr != auth_pong.client_addr
                            || pong.tunnel_addr != auth_pong.tunnel_addr =>
                    {
                        return Err(SetupError::AttemptingToAuthWithOldFlow);
                    }
                    // Queued requests retain the same deadline and continue receiving events.
                    _ => {}
                }
            }
        }

        Err(SetupError::FailedToConnect)
    }

    pub async fn send<M: MessageEncoding>(&mut self, msg: &M) -> std::io::Result<()> {
        self.buffer.clear();
        msg.write_to(&mut self.buffer)?;
        let sent = self
            .packet_io
            .send_to(&self.buffer, self.control_addr)
            .await?;
        if sent != self.buffer.len() {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<ControlFeed, ControlError> {
        if let Some(feed) = self.pending.pop_front() {
            return Ok(feed);
        }
        self.recv_wire().await
    }

    async fn recv_wire(&mut self) -> Result<ControlFeed, ControlError> {
        self.buffer.resize(65_536, 0);

        let (bytes, remote) = self.packet_io.recv_from(&mut self.buffer).await?;
        if remote != self.control_addr {
            return Err(ControlError::InvalidRemote {
                expected: self.control_addr,
                got: remote,
            });
        }

        let mut reader = &self.buffer[..bytes];
        let feed =
            ControlFeed::read_from(&mut reader).map_err(ControlError::FailedToReadControlFeed)?;

        Ok(feed)
    }
}
