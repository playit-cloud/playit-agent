use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::Instant;

use playit_agent_proto::control_feed::{ControlFeed, NewClient};
use playit_agent_proto::control_messages::{ControlResponse, UdpChannelDetails};

use crate::agent_control::errors::TryTimeoutHelper;
use crate::agent_control::established_control::EstablishedControl;
use crate::utils::now_milli;

use super::address_selector::AddressSelector;
use super::connected_control::ConnectedControl;
use super::errors::SetupError;
use super::{AuthResource, PacketIO};

pub struct MaintainedControl<I: PacketIO, A: AuthResource> {
    control: EstablishedControl<A, I>,
    next_keep_alive: Instant,
    next_ping: Instant,
    last_pong: Instant,
    last_udp_auth: Option<Instant>,
    udp_request_id: Option<u64>,
    last_control_targets: Vec<SocketAddr>,
}

impl<I: PacketIO, A: AuthResource> MaintainedControl<I, A> {
    pub async fn setup(io: I, auth: A) -> Result<Self, SetupError> {
        let addresses = auth.get_control_addresses().await?;
        let setup = AddressSelector::new(addresses.clone(), io)
            .connect_to_first()
            .try_timeout(Duration::from_secs(10))
            .await?;

        let control_channel = setup
            .auth_into_established(auth)
            .try_timeout(Duration::from_secs(10))
            .await?;

        Ok(MaintainedControl {
            control: control_channel,
            next_keep_alive: Instant::now(),
            next_ping: Instant::now(),
            last_pong: Instant::now(),
            last_udp_auth: None,
            udp_request_id: None,
            last_control_targets: addresses,
        })
    }

    pub async fn reload_control_addr<E: Into<SetupError>, C: Future<Output = Result<I, E>>>(
        &mut self,
        create_io: C,
    ) -> Result<bool, SetupError> {
        let addresses = self
            .control
            .auth
            .get_control_addresses()
            .try_timeout(Duration::from_secs(5))
            .await?;

        if self.last_control_targets == addresses
            && self.last_pong.elapsed() < Duration::from_secs(6)
        {
            return Ok(false);
        }

        let new_io = async { create_io.await.map_err(|e| e.into()) }
            .try_timeout(Duration::from_secs(5))
            .await?;

        let connected = AddressSelector::new(addresses.clone(), new_io)
            .connect_to_first()
            .try_timeout(Duration::from_secs(10))
            .await?;

        let updated = self
            .replace_connection(connected, true)
            .try_timeout(Duration::from_secs(5))
            .await?;

        self.last_control_targets = addresses;
        Ok(updated)
    }

    pub async fn replace_connection(
        &mut self,
        mut connected: ConnectedControl<I>,
        force: bool,
    ) -> Result<bool, SetupError> {
        if !force
            && self.control.conn.pong_latest.client_addr == connected.pong_latest.client_addr
            && self.control.conn.pong_latest.tunnel_addr == connected.pong_latest.tunnel_addr
        {
            return Ok(false);
        }

        let registered = connected
            .authenticate(&self.control.auth)
            .try_timeout(Duration::from_secs(10))
            .await?;

        tracing::info!(old = %self.control.conn.pong_latest.tunnel_addr, new = %connected.pong_latest.tunnel_addr, "update control address");
        self.control.replace_connection(connected, registered);
        self.reset_timers();

        Ok(true)
    }

    fn reset_timers(&mut self) {
        self.next_ping = Instant::now();
        self.next_keep_alive = Instant::now();
        self.last_pong = Instant::now();
        self.last_udp_auth = None;
        self.udp_request_id = None;
    }

    pub async fn send_udp_session_auth(&mut self, min_wait: Duration) -> bool {
        if self.last_udp_auth.is_some_and(|at| at.elapsed() < min_wait) {
            return false;
        }
        let id = super::next_request_id();
        match self
            .control
            .send_setup_udp_channel(id)
            .try_timeout(Duration::from_secs(1))
            .await
        {
            Ok(()) => {
                self.last_udp_auth = Some(Instant::now());
                self.udp_request_id = Some(id);
                true
            }
            Err(error) => {
                tracing::debug!(?error, "UDP session request failed");
                false
            }
        }
    }

    pub async fn update(&mut self) -> Option<TunnelControlEvent> {
        if self.last_pong.elapsed() >= Duration::from_secs(6) {
            self.control.set_expired();
        }
        if self.control.is_expired().is_some() {
            if let Err(error) = self
                .control
                .authenticate()
                .try_timeout(Duration::from_secs(5))
                .await
            {
                tracing::debug!(?error, "Control authentication failed");
                tokio::time::sleep(Duration::from_secs(2)).await;
                return None;
            }
            self.reset_timers();
        }

        if Instant::now() >= self.next_ping {
            self.next_ping = Instant::now() + Duration::from_secs(1);
            let id = super::next_request_id();
            if let Err(error) = self
                .control
                .send_ping(id, now_milli())
                .try_timeout(Duration::from_secs(1))
                .await
            {
                tracing::debug!(?error, "Control ping failed");
            }
        }
        if Instant::now() >= self.next_keep_alive {
            let remaining = self.control.get_expire_at().saturating_sub(now_milli());
            self.next_keep_alive =
                Instant::now() + Duration::from_secs(if remaining < 30_000 { 10 } else { 60 });
            let id = super::next_request_id();
            if let Err(error) = self
                .control
                .send_keep_alive(id)
                .try_timeout(Duration::from_secs(1))
                .await
            {
                tracing::debug!(?error, "Control keep-alive failed");
            }
        }

        match tokio::time::timeout(Duration::from_millis(100), self.control.recv_feed_msg()).await {
            Ok(Ok(ControlFeed::NewClient(client))) => Some(TunnelControlEvent::NewClient(client)),
            Ok(Ok(ControlFeed::NewClientOld(client))) => {
                Some(TunnelControlEvent::NewClient(client.into()))
            }
            Ok(Ok(ControlFeed::Response(response))) => {
                match response.content {
                    ControlResponse::Pong(_) => self.last_pong = Instant::now(),
                    ControlResponse::Unauthorized
                        if self.udp_request_id == Some(response.request_id)
                            || self.control.pending_keep_alive == Some(response.request_id)
                            || self
                                .control
                                .pending_ping
                                .is_some_and(|(id, _, _)| id == response.request_id) =>
                    {
                        self.control.set_expired()
                    }
                    ControlResponse::UdpChannelDetails(details)
                        if self.udp_request_id == Some(response.request_id) =>
                    {
                        self.udp_request_id = None;
                        return Some(TunnelControlEvent::UdpChannelDetails(details));
                    }
                    _ => {}
                }
                None
            }
            Ok(Err(error)) => {
                tracing::debug!(?error, "Control receive failed");
                None
            }
            Err(_) => None,
        }
    }
}

pub enum TunnelControlEvent {
    NewClient(NewClient),
    UdpChannelDetails(UdpChannelDetails),
}
