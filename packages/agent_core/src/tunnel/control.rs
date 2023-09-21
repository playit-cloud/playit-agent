use std::net::SocketAddr;

use playit_agent_proto::control_feed::ControlFeed;
use playit_agent_proto::control_messages::{AgentRegistered, ControlRequest, ControlResponse, Ping, Pong};
use playit_agent_proto::encoding::MessageEncoding;
use playit_agent_proto::rpc::ControlRpcMessage;

use crate::api::PlayitApi;
use crate::tunnel::setup::{ConnectedControl, SetupError};
use crate::utils::now_milli;

pub struct AuthenticatedControl {
    pub(crate) secret_key: String,
    pub(crate) api_client: PlayitApi,
    pub(crate) conn: ConnectedControl,
    pub(crate) last_pong: Pong,
    pub(crate) registered: AgentRegistered,
    pub(crate) buffer: Vec<u8>,
    pub(crate) current_ping: Option<u32>,

    pub(crate) force_expired: bool,
}

impl AuthenticatedControl {
    pub async fn send_keep_alive(&mut self, request_id: u64) -> Result<(), ControlError> {
        self.send(ControlRpcMessage {
            request_id,
            content: ControlRequest::AgentKeepAlive(self.registered.id.clone()),
        }).await
    }

    pub async fn send_setup_udp_channel(&mut self, request_id: u64) -> Result<(), ControlError> {
        self.send(ControlRpcMessage {
            request_id,
            content: ControlRequest::SetupUdpChannel(self.registered.id.clone()),
        }).await
    }

    pub async fn send_ping(&mut self, request_id: u64, now: u64) -> Result<(), ControlError> {
        self.send(ControlRpcMessage {
            request_id,
            content: ControlRequest::Ping(Ping { now, current_ping: self.current_ping, session_id: Some(self.registered.id.clone()) }),
        }).await
    }

    pub fn get_expire_at(&self) -> u64 {
        self.registered.expires_at
    }

    pub fn is_expired(&self) -> bool {
        self.force_expired || self.last_pong.session_expire_at.is_none() || self.flow_changed()
    }

    pub fn set_expired(&mut self) {
        self.force_expired = true;
    }

    fn flow_changed(&self) -> bool {
        self.conn.pong.client_addr != self.last_pong.client_addr
    }

    async fn send(&mut self, req: ControlRpcMessage<ControlRequest>) -> Result<(), ControlError> {
        self.buffer.clear();
        req.write_to(&mut self.buffer)?;
        self.conn.udp.send_to(&self.buffer, self.conn.control_addr).await?;
        Ok(())
    }

    pub async fn authenticate(&mut self) -> Result<(), SetupError> {
        let conn = ConnectedControl {
            control_addr: self.conn.control_addr,
            udp: self.conn.udp.clone(),
            pong: self.last_pong.clone(),
        };

        let res = conn.authenticate(
            self.api_client.get_client().api_base().to_string(),
            self.secret_key.clone()
        ).await?;

        *self = res;

        Ok(())
    }

    pub fn into_requires_auth(self) -> ConnectedControl {
        ConnectedControl {
            control_addr: self.conn.control_addr,
            udp: self.conn.udp,
            pong: self.last_pong,
        }
    }

    pub async fn recv_feed_msg(&mut self) -> Result<ControlFeed, ControlError> {
        self.buffer.resize(1024, 0);
        let (bytes, remote) = self.conn.udp.recv_from(&mut self.buffer).await?;
        if remote != self.conn.control_addr {
            return Err(ControlError::InvalidRemote { expected: self.conn.control_addr, got: remote });
        }

        let mut reader = &self.buffer[..bytes];
        let feed = ControlFeed::read_from(&mut reader).map_err(|e| ControlError::FailedToReadControlFeed(e))?;

        if let ControlFeed::Response(res) = &feed {
            match &res.content {
                ControlResponse::AgentRegistered(registered) => {
                    tracing::info!(details = ?registered, "agent registered");
                    self.registered = registered.clone();
                }
                ControlResponse::Pong(pong) => {
                    self.current_ping = Some((now_milli() - pong.request_now) as u32);
                    self.last_pong = pong.clone();

                    if let Some(expires_at) = pong.session_expire_at {
                        self.registered.expires_at = expires_at;
                    }
                }
                _ => {}
            }
        }

        Ok(feed)
    }
}

#[derive(Debug)]
pub enum ControlError {
    IoError(std::io::Error),
    InvalidRemote { expected: SocketAddr, got: SocketAddr },
    FailedToReadControlFeed(std::io::Error),
}

impl From<std::io::Error> for ControlError {
    fn from(e: std::io::Error) -> Self {
        ControlError::IoError(e)
    }
}