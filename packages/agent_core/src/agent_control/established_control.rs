use playit_agent_proto::control_feed::ControlFeed;
use playit_agent_proto::control_messages::{AgentRegistered, ControlRequest, ControlResponse, Ping, Pong};
use playit_agent_proto::rpc::ControlRpcMessage;

use crate::utils::now_milli;

use super::connected_control::ConnectedControl;
use super::errors::{ControlError, SetupError};
use super::{AuthResource, PacketIO};

pub struct EstablishedControl<A: AuthResource, IO: PacketIO> {
    pub(super) auth: A,
    pub(super) conn: ConnectedControl<IO>,
    pub(super) pong_at_auth: Pong,
    pub(super) registered: AgentRegistered,
    pub(super) current_ping: Option<u32>,
    pub(super) clock_offset: i64,
    pub(super) force_expired: bool,
}

impl<A: AuthResource, IO: PacketIO> EstablishedControl<A, IO> {
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

    pub fn is_expired(&self) -> Option<ExpiredReason> {
        if self.force_expired {
            return Some(ExpiredReason::Forced);
        }
        if self.pong_at_auth.session_expire_at.is_none() {
            return Some(ExpiredReason::SessionNotSetup);
        }
        if self.flow_changed() {
            return Some(ExpiredReason::FlowChanged);
        }
        None
    }

    pub fn set_expired(&mut self) {
        self.force_expired = true;
    }

    fn flow_changed(&self) -> bool {
        self.conn.pong_latest.client_addr != self.pong_at_auth.client_addr 
            || self.conn.pong_latest.tunnel_addr != self.pong_at_auth.tunnel_addr
    }

    async fn send(&mut self, req: ControlRpcMessage<ControlRequest>) -> Result<(), ControlError> {
        self.conn.send(&req).await?;
        Ok(())
    }

    pub async fn authenticate(&mut self) -> Result<(), SetupError> {
        let registered = self.conn.authenticate(&self.auth).await?;

        self.force_expired = false;
        self.registered = registered;
        self.pong_at_auth = self.conn.pong_latest.clone();

        tracing::info!(
            last_pong = ?self.pong_at_auth,
            "authenticate control"
        );

        Ok(())
    }

    pub fn into_connected(self) -> ConnectedControl<IO> {
        self.conn
    }

    pub async fn recv_feed_msg(&mut self) -> Result<ControlFeed, ControlError> {
        let feed = self.conn.recv().await?;
        
        if let ControlFeed::Response(res) = &feed {
            match &res.content {
                ControlResponse::AgentRegistered(registered) => {
                    tracing::info!(details = ?registered, "agent registered");
                    self.registered = registered.clone();
                }
                ControlResponse::Pong(pong) => {
                    let now = now_milli();
                    let rtt = (now.max(pong.request_now) - pong.request_now) as u32;

                    let server_ts = pong.server_now - (rtt / 2) as u64;
                    let local_ts = pong.request_now;
                    self.clock_offset = local_ts as i64 - server_ts as i64;

                    if 10_000 < self.clock_offset.abs() {
                        tracing::warn!(offset = self.clock_offset, "local timestamp if over 10 seconds off");
                    }

                    self.current_ping = Some(rtt);

                    if let Some(expires_at) = pong.session_expire_at {
                        /* normalize to local timestamp to handle when host clock is wrong */
                        self.registered.expires_at = pong.request_now + (expires_at - pong.server_now).max(rtt as u64) - rtt as u64;
                    }
                }
                _ => {}
            }
        }

        Ok(feed)
    }
}


#[derive(Debug, PartialEq, Eq)]
pub enum ExpiredReason {
    Forced,
    SessionNotSetup,
    FlowChanged,
}