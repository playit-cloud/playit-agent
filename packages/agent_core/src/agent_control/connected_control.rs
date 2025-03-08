use std::{net::SocketAddr, time::Duration};

use message_encoding::MessageEncoding;
use playit_agent_proto::{control_feed::ControlFeed, control_messages::{AgentRegistered, ControlRequest, ControlResponse, Ping, Pong}, raw_slice::RawSlice, rpc::ControlRpcMessage};

use crate::utils::now_milli;

use super::{errors::{ControlError, SetupError}, established_control::EstablishedControl, AuthResource, PacketIO};

#[derive(Debug)]
pub struct ConnectedControl<IO: PacketIO> {
    pub(super) control_addr: SocketAddr,
    pub(super) packet_io: IO,
    pub(super) pong_latest: Pong,
    pub(super) buffer: Vec<u8>,
}

impl<IO: PacketIO> ConnectedControl<IO> {
    pub fn new(control_addr: SocketAddr, udp: IO, pong: Pong) -> Self {
        ConnectedControl { control_addr, packet_io: udp, pong_latest: pong, buffer: Vec::with_capacity(1024) }
    }

    pub fn control_addr(&self) -> SocketAddr {
        self.control_addr
    }

    pub fn pong(&self) -> Pong {
        self.pong_latest.clone()
    }

    pub async fn auth_into_established<A: AuthResource>(mut self, auth: A) -> Result<EstablishedControl<A, IO>, SetupError> {
        let registered = self.authenticate(&auth).await?;
        Ok(self.into_established(auth, registered))
    }

    pub fn into_established<A: AuthResource>(self, auth: A, registered: AgentRegistered) -> EstablishedControl<A, IO> {
        let pong = self.pong_latest.clone();

        EstablishedControl {
            auth,
            conn: self,
            pong_at_auth: pong,
            registered,
            current_ping: None,
            clock_offset: 0,
            force_expired: false,
        }
    }

    pub fn reset_established<A: AuthResource>(self, established: &mut EstablishedControl<A, IO>, registered: AgentRegistered) {
        established.registered = registered;
        established.pong_at_auth = self.pong_latest.clone();
        established.conn = self;
        established.current_ping = None;
        established.force_expired = false;
    }

    pub async fn authenticate<A: AuthResource>(&mut self, auth: &A) -> Result<AgentRegistered, SetupError> {
        let auth_pong = self.pong_latest.clone();
        let res = auth.authenticate(&auth_pong).await?;

        let bytes = match hex::decode(&res.key) {
            Ok(data) => data,
            Err(_) => return Err(SetupError::FailedToDecodeSignedAgentRegisterHex),
        };

        let request_id = now_milli();

        for _ in 0..5 {
            self.send(&ControlRpcMessage {
                request_id,
                content: RawSlice(&bytes),
            }).await?;

            for _ in 0..5 {
                let mesage = match tokio::time::timeout(Duration::from_millis(500), self.recv()).await {
                    Ok(Ok(msg)) => msg,
                    Ok(Err(error)) => {
                        tracing::error!(?error, "got error reading from socket");
                        break;
                    }
                    Err(_) => {
                        tracing::error!("timeout waiting for register response");
                        continue;
                    }
                };

                let response = match mesage {
                    ControlFeed::Response(response) if response.request_id == request_id => response,
                    other => {
                        tracing::error!(?other, "got unexpected response from register request");
                        continue;
                    }
                };

                return match response.content {
                    ControlResponse::AgentRegistered(registered) => Ok(registered),
                    ControlResponse::InvalidSignature => Err(SetupError::RegisterInvalidSignature),
                    ControlResponse::Unauthorized => {
                        /* most likely due to a changed client addr, send pong to refresh value */
                        let _ = self.send(&ControlRpcMessage {
                            request_id,
                            content: ControlRequest::Ping(Ping {
                                now: now_milli(),
                                current_ping: None,
                                session_id: None,
                            }),
                        }).await;

                        Err(SetupError::RegisterUnauthorized)
                    },
                    ControlResponse::Pong(pong) => {
                        if pong.client_addr != auth_pong.client_addr || pong.tunnel_addr != auth_pong.tunnel_addr {
                            Err(SetupError::AttemptingToAuthWithOldFlow)
                        } else {
                            continue;
                        }
                    }
                    ControlResponse::RequestQueued => {
                        tracing::info!("register queued, waiting 1s");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        break;
                    }
                    other => {
                        tracing::error!(?other, "expected AgentRegistered but got something else");
                        continue;
                    }
                };
            }
        }

        Err(SetupError::FailedToConnect)
    }

    pub async fn send<M: MessageEncoding>(&mut self, msg: &M) -> std::io::Result<()> {
        self.buffer.clear();
        msg.write_to(&mut self.buffer)?;
        self.packet_io.send_to(&self.buffer, self.control_addr).await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<ControlFeed, ControlError> {
        self.buffer.resize(1024, 0);

        let (bytes, remote) = self.packet_io.recv_from(&mut self.buffer).await?;
        if remote != self.control_addr {
            return Err(ControlError::InvalidRemote { expected: self.control_addr, got: remote });
        }

        let mut reader = &self.buffer[..bytes];
        let feed = ControlFeed::read_from(&mut reader).map_err(|e| ControlError::FailedToReadControlFeed(e))?;

        if let ControlFeed::Response(ControlRpcMessage { content: ControlResponse::Pong(pong), .. }) = &feed {
            self.pong_latest = pong.clone();
        }

        Ok(feed)
    }
}

