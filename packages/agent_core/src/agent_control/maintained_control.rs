use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

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
    last_keep_alive: u64,
    last_ping: u64,
    last_pong: u64,
    last_udp_auth: u64,
    last_control_targets: Vec<SocketAddr>,
    connection_state: ControlConnectionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlConnectionState {
    Connecting,
    Connected,
    Reconnecting { since: u64 },
    Offline { since: u64 },
}

const OFFLINE_ERROR_AFTER_MS: u64 = 5 * 60 * 1_000;

impl ControlConnectionState {
    fn disconnected(self, now_ms: u64) -> (Self, bool) {
        if self == Self::Connected {
            (Self::Reconnecting { since: now_ms }, true)
        } else {
            (self, false)
        }
    }

    fn connected(self) -> (Self, Option<u64>) {
        let outage_started = match self {
            Self::Reconnecting { since } | Self::Offline { since } => Some(since),
            Self::Connecting | Self::Connected => None,
        };
        (Self::Connected, outage_started)
    }

    fn escalated(self, now_ms: u64) -> (Self, bool) {
        match self {
            Self::Reconnecting { since }
                if now_ms.saturating_sub(since) >= OFFLINE_ERROR_AFTER_MS =>
            {
                (Self::Offline { since }, true)
            }
            _ => (self, false),
        }
    }
}

impl<I: PacketIO, A: AuthResource> MaintainedControl<I, A> {
    pub async fn setup(io: I, auth: A) -> Result<Self, SetupError> {
        let addresses = auth.get_control_addresses().await?;
        let setup = AddressSelector::new(addresses.clone(), io)
            .connect_to_first()
            .try_timeout(Duration::from_secs(10))
            .await?;

        let control_addr = setup.control_addr();
        let initial_pong = setup.pong();
        let initial_rtt = now_milli().saturating_sub(initial_pong.request_now);
        let control_channel = setup
            .auth_into_established(auth)
            .try_timeout(Duration::from_secs(10))
            .await?;

        let mut maintained = MaintainedControl {
            control: control_channel,
            last_keep_alive: 0,
            last_ping: 0,
            last_pong: now_milli(),
            last_udp_auth: 0,
            last_control_targets: addresses,
            connection_state: ControlConnectionState::Connecting,
        };
        maintained.mark_connected(control_addr, initial_rtt);
        Ok(maintained)
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

        if self.last_control_targets == addresses {
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
            .replace_connection(connected, false)
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
            && self.control.conn.pong_latest.client_addr.ip()
                == connected.pong_latest.client_addr.ip()
            && self.control.conn.pong_latest.tunnel_addr == connected.pong_latest.tunnel_addr
        {
            return Ok(false);
        }

        let registered = connected
            .authenticate(&self.control.auth)
            .try_timeout(Duration::from_secs(10))
            .await?;

        tracing::debug!(old = %self.control.conn.pong_latest.tunnel_addr, new = %connected.pong_latest.tunnel_addr, "Updated control address");
        connected.reset_established(&mut self.control, registered);

        Ok(true)
    }

    pub async fn send_udp_session_auth(&mut self, now_ms: u64, min_wait_ms: u64) -> bool {
        if now_ms < self.last_udp_auth + min_wait_ms {
            return false;
        }

        self.last_udp_auth = now_ms;
        if let Err(error) = self
            .control
            .send_setup_udp_channel(1)
            .try_timeout(Duration::from_secs(5))
            .await
        {
            tracing::debug!(?error, "Failed to send UDP channel setup request");
        }

        true
    }

    pub async fn update(&mut self) -> Option<TunnelControlEvent> {
        if let Some(reason) = self.control.is_expired() {
            if reason == super::established_control::ExpiredReason::SessionNotSetup {
                tracing::debug!("Waiting for the initial authenticated pong");
            } else {
                self.mark_disconnected();
                if let Err(error) = self
                    .control
                    .authenticate()
                    .try_timeout(Duration::from_secs(5))
                    .await
                {
                    tracing::debug!(?error, "Failed to reauthenticate control session");
                    self.maybe_mark_offline();
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    return None;
                }

                self.last_pong = now_milli();
                let addr = self.control.conn.control_addr();
                let ping = self.control.current_ping.unwrap_or_default() as u64;
                self.mark_connected(addr, ping);
            }
        }

        let now = now_milli();
        if now - self.last_ping > 1_000 {
            self.last_ping = now;

            if let Err(error) = self
                .control
                .send_ping(200, now)
                .try_timeout(Duration::from_secs(1))
                .await
            {
                tracing::debug!(?error, "Failed to send ping");
            }
        }

        let time_till_expire = self.control.get_expire_at().max(now) - now;
        tracing::trace!(time_till_expire, "time till expire");

        /* keep alive every 60s or every 10s if expiring soon */
        let interval = if time_till_expire < 30_000 {
            10_000
        } else {
            60_000
        };

        if interval < now - self.last_keep_alive {
            self.last_keep_alive = now;

            tracing::debug!(time_till_expire, "send KeepAlive");
            if let Err(error) = self
                .control
                .send_keep_alive(100)
                .try_timeout(Duration::from_secs(1))
                .await
            {
                tracing::debug!(?error, "Failed to send keep-alive");
            }
        }

        let mut timeouts = 0;

        for _ in 0..30 {
            match tokio::time::timeout(Duration::from_millis(100), self.control.recv_feed_msg())
                .await
            {
                Ok(Ok(ControlFeed::NewClient(new_client))) => {
                    return Some(TunnelControlEvent::NewClient(new_client));
                }
                Ok(Ok(ControlFeed::NewClientOld(new_client))) => {
                    return Some(TunnelControlEvent::NewClient(new_client.into()));
                }
                Ok(Ok(ControlFeed::Response(msg))) => match msg.content {
                    ControlResponse::UdpChannelDetails(details) => {
                        return Some(TunnelControlEvent::UdpChannelDetails(details));
                    }
                    ControlResponse::Unauthorized => {
                        tracing::debug!("session no longer authorized");
                        self.control.set_expired();
                    }
                    ControlResponse::Pong(pong) => {
                        self.last_pong = now_milli();

                        if pong.client_addr != self.control.pong_at_auth.client_addr {
                            tracing::debug!(
                                new_client = %pong.client_addr,
                                old_client = %self.control.pong_at_auth.client_addr,
                                "client ip changed"
                            );
                        }
                    }
                    msg => {
                        tracing::debug!(?msg, "got response");
                    }
                },
                Ok(Err(error)) => {
                    tracing::debug!(?error, "Failed to parse control response");
                }
                Err(_) => {
                    timeouts += 1;

                    if timeouts >= 10 {
                        tracing::trace!("feed recv timeout");
                        break;
                    }
                }
            }
        }

        if self.last_pong != 0 && now_milli() - self.last_pong > 6_000 {
            self.mark_disconnected();
            self.last_pong = 0;
            self.control.set_expired();
        }

        self.maybe_mark_offline();
        None
    }

    fn mark_disconnected(&mut self) {
        let (state, changed) = self.connection_state.disconnected(now_milli());
        self.connection_state = state;
        if changed {
            tracing::warn!("Lost connection to playit.gg, reconnecting");
        }
    }

    fn mark_connected(&mut self, address: SocketAddr, rtt_ms: u64) {
        let previous = self.connection_state;
        let (state, outage_started) = previous.connected();
        self.connection_state = state;

        match previous {
            ControlConnectionState::Connecting => {
                tracing::info!("Connected to playit.gg (tunnel server {address}, ping {rtt_ms}ms)")
            }
            ControlConnectionState::Reconnecting { since }
            | ControlConnectionState::Offline { since } => {
                let offline_secs =
                    now_milli().saturating_sub(outage_started.unwrap_or(since)) / 1_000;
                tracing::info!(
                    "Reconnected to playit.gg (was offline {offline_secs}s, tunnel server {address}, ping {rtt_ms}ms)"
                );
            }
            ControlConnectionState::Connected => {}
        }
    }

    fn maybe_mark_offline(&mut self) {
        let (state, changed) = self.connection_state.escalated(now_milli());
        self.connection_state = state;
        if changed {
            tracing::error!(
                "Still unable to reach playit.gg after 5m. Check your internet connection and firewall rules for UDP port 5525"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ControlConnectionState, OFFLINE_ERROR_AFTER_MS};

    #[test]
    fn connection_state_emits_one_loss_and_one_escalation() {
        let (state, lost) = ControlConnectionState::Connected.disconnected(10);
        assert!(lost);
        let (state, lost_again) = state.disconnected(20);
        assert!(!lost_again);

        let (state, early) = state.escalated(10 + OFFLINE_ERROR_AFTER_MS - 1);
        assert!(!early);
        let (state, escalated) = state.escalated(10 + OFFLINE_ERROR_AFTER_MS);
        assert!(escalated);
        let (_, escalated_again) = state.escalated(10 + OFFLINE_ERROR_AFTER_MS + 1);
        assert!(!escalated_again);
    }

    #[test]
    fn connection_state_retains_outage_start_for_recovery() {
        let (state, _) = ControlConnectionState::Connected.disconnected(42);
        let (state, since) = state.connected();
        assert_eq!(state, ControlConnectionState::Connected);
        assert_eq!(since, Some(42));
    }
}

pub enum TunnelControlEvent {
    NewClient(NewClient),
    UdpChannelDetails(UdpChannelDetails),
}
