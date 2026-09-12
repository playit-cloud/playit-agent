use std::{collections::BTreeMap, time::Duration};
use tokio::time::Instant;

use playit_agent_proto::control_feed::ControlFeed;
use playit_agent_proto::control_messages::{
    AgentRegistered, CheckMtuReceived, CheckMtuReceivedAck, ControlRequest, ControlResponse,
    MtuTestFail, MtuTestFailCode, MtuTestPacket, Ping, Pong, SendMtuTest,
};
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
    pub(super) lease: SessionLease,
    pub(super) pending_keep_alive: Option<u64>,
    pub(super) pending_ping: Option<(u64, u64, Instant)>,
    pub(super) pending_mtu_data: MtuData,
    pub(super) known_mtu_data: MtuData,
}

pub(super) struct SessionLease {
    deadline: Instant,
    forced: bool,
}

impl SessionLease {
    pub(super) fn new(registered: &AgentRegistered, pong: &Pong, received_at: Instant) -> Self {
        Self {
            deadline: received_at
                .checked_add(Duration::from_millis(
                    registered.expires_at.saturating_sub(pong.server_now),
                ))
                .unwrap_or(received_at),
            forced: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MtuData {
    pub largest_received_packet: Option<CheckMtuReceivedAck>,
    pub largest_payload_by_datacenter: BTreeMap<u32, u32>,
    pub latest_test_fail: Option<MtuTestFail>,
}

impl MtuData {
    fn record_check_mtu_received_ack(&mut self, ack: CheckMtuReceivedAck) {
        let should_replace = match self.largest_received_packet.as_ref() {
            Some(current) => ack.message_length >= current.message_length,
            None => true,
        };

        if should_replace {
            self.largest_received_packet = Some(ack);
        }
    }

    fn record_mtu_test_packet(&mut self, packet: MtuTestPacket) {
        self.largest_payload_by_datacenter
            .entry(packet.data_center_id)
            .and_modify(|current| *current = (*current).max(packet.udp_payload_length))
            .or_insert(packet.udp_payload_length);
    }

    fn record_mtu_test_fail(&mut self, fail: MtuTestFail) {
        self.latest_test_fail = Some(fail);
    }
}

impl<A: AuthResource, IO: PacketIO> EstablishedControl<A, IO> {
    pub(super) fn new(auth: A, conn: ConnectedControl<IO>, registered: AgentRegistered) -> Self {
        let lease = SessionLease::new(&registered, &conn.pong_latest, conn.pong_received_at);
        Self {
            auth,
            pong_at_auth: conn.pong_latest.clone(),
            conn,
            lease,
            registered,
            current_ping: None,
            clock_offset: 0,
            pending_ping: None,
            pending_keep_alive: None,
            pending_mtu_data: MtuData::default(),
            known_mtu_data: MtuData::default(),
        }
    }

    pub(super) fn replace_connection(
        &mut self,
        conn: ConnectedControl<IO>,
        registered: AgentRegistered,
    ) {
        *self = Self::new(self.auth.clone(), conn, registered);
    }

    pub async fn send_keep_alive(&mut self, request_id: u64) -> Result<(), ControlError> {
        self.pending_keep_alive = Some(request_id);
        self.send(ControlRpcMessage {
            request_id,
            content: ControlRequest::AgentKeepAlive(self.registered.id.clone()),
        })
        .await
    }

    pub async fn send_setup_udp_channel(&mut self, request_id: u64) -> Result<(), ControlError> {
        self.send(ControlRpcMessage {
            request_id,
            content: ControlRequest::SetupUdpChannel(self.registered.id.clone()),
        })
        .await
    }

    pub async fn send_ping(&mut self, request_id: u64, now: u64) -> Result<(), ControlError> {
        if self
            .pending_ping
            .is_some_and(|(_, _, sent)| sent.elapsed() < Duration::from_secs(3))
        {
            return Ok(());
        }
        self.pending_ping = Some((request_id, now, Instant::now()));
        self.send(ControlRpcMessage {
            request_id,
            content: ControlRequest::Ping(Ping {
                now,
                current_ping: self.current_ping,
                session_id: Some(self.registered.id.clone()),
            }),
        })
        .await
    }

    pub async fn send_check_mtu_received(
        &mut self,
        request_id: u64,
        id: u64,
        message_size: u32,
    ) -> Result<(), ControlError> {
        self.send(ControlRpcMessage {
            request_id,
            content: ControlRequest::CheckMtuReceived(CheckMtuReceived { id, message_size }),
        })
        .await
    }

    pub async fn send_mtu_test(
        &mut self,
        request_id: u64,
        id: u64,
        data_center_id: u32,
        udp_payload_length: u32,
    ) -> Result<(), ControlError> {
        self.send(ControlRpcMessage {
            request_id,
            content: ControlRequest::SendMtuTest(SendMtuTest {
                id,
                data_center_id,
                udp_payload_length,
            }),
        })
        .await
    }

    pub fn get_expire_at(&self) -> u64 {
        now_milli().saturating_add(
            self.lease
                .deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64,
        )
    }

    pub fn is_expired(&self) -> Option<ExpiredReason> {
        if self.lease.forced {
            return Some(ExpiredReason::Forced);
        }
        if self.lease.deadline <= Instant::now() {
            return Some(ExpiredReason::Deadline);
        }
        if self.flow_changed() {
            return Some(ExpiredReason::FlowChanged);
        }
        None
    }

    pub fn set_expired(&mut self) {
        self.lease.forced = true;
    }

    pub fn pending_mtu_data(&self) -> &MtuData {
        &self.pending_mtu_data
    }

    pub fn known_mtu_data(&self) -> &MtuData {
        &self.known_mtu_data
    }

    pub fn commit_pending_mtu_data(&mut self) {
        self.known_mtu_data = self.pending_mtu_data.clone();
    }

    pub fn clear_pending_mtu_data(&mut self) {
        self.pending_mtu_data = MtuData::default();
    }

    pub fn clear_known_mtu_data(&mut self) {
        self.known_mtu_data = MtuData::default();
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
        self.conn.refresh_pong().await?;
        let registered = self.conn.authenticate(&self.auth).await?;

        self.lease = SessionLease::new(
            &registered,
            &self.conn.pong_latest,
            self.conn.pong_received_at,
        );
        self.pending_ping = None;
        self.pending_keep_alive = None;
        self.registered = registered;
        self.pong_at_auth = self.conn.pong_latest.clone();

        tracing::debug!(
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
                ControlResponse::AgentRegistered(registered)
                    if self.pending_keep_alive == Some(res.request_id) =>
                {
                    self.pending_keep_alive = None;
                    self.lease = SessionLease::new(
                        registered,
                        &self.conn.pong_latest,
                        self.conn.pong_received_at,
                    );
                    self.registered = registered.clone();
                }
                ControlResponse::Pong(pong) => {
                    let Some((id, sent_now, sent_at)) = self.pending_ping else {
                        return Err(ControlError::UnmatchedResponse);
                    };
                    if res.request_id != id || pong.request_now != sent_now {
                        return Err(ControlError::UnmatchedResponse);
                    }
                    self.pending_ping = None;
                    let rtt = sent_at.elapsed().as_millis().min(u32::MAX as u128) as u32;
                    self.current_ping = Some(rtt);
                    self.conn.pong_latest = pong.clone();
                    self.conn.pong_received_at = Instant::now();
                    let offset = i128::from(pong.request_now) - i128::from(pong.server_now)
                        + i128::from(rtt / 2);
                    self.clock_offset = offset.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
                    match pong.session_expire_at {
                        Some(expires_at) => {
                            let remaining = expires_at
                                .saturating_sub(pong.server_now)
                                .saturating_sub(u64::from(rtt / 2));
                            self.lease.deadline = Instant::now()
                                .checked_add(Duration::from_millis(remaining))
                                .unwrap_or_else(Instant::now);
                        }
                        None => self.lease.forced = true,
                    }
                }
                ControlResponse::CheckMtuReceivedAck(ack) => {
                    self.pending_mtu_data
                        .record_check_mtu_received_ack(ack.clone());
                }
                ControlResponse::MtuTestPacket(packet) => {
                    self.pending_mtu_data.record_mtu_test_packet(packet.clone());
                }
                ControlResponse::MtuTestFail(fail) => {
                    self.pending_mtu_data.record_mtu_test_fail(fail.clone());
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
    Deadline,
    FlowChanged,
}

impl MtuData {
    pub fn latest_test_fail_code(&self) -> Option<MtuTestFailCode> {
        self.latest_test_fail.as_ref().map(|fail| fail.error_code)
    }
}
