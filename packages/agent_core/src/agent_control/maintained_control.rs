use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use playit_agent_proto::control_feed::{ControlFeed, NewClient};
use playit_agent_proto::control_messages::{ControlResponse, UdpChannelDetails};

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
}

impl<I: PacketIO, A: AuthResource> MaintainedControl<I, A> {
    pub async fn setup(io: I, auth: A) -> Result<Self, SetupError> {
        let addresses = auth.get_control_addresses().await?;
        let setup = AddressSelector::new(addresses.clone(), io).connect_to_first().await?;
        let control_channel = setup.auth_into_established(auth).await?;

        Ok(MaintainedControl {
            control: control_channel,
            last_keep_alive: 0,
            last_ping: 0,
            last_pong: 0,
            last_udp_auth: 0,
            last_control_targets: addresses,
        })
    }

    pub async fn reload_control_addr<E: Into<SetupError>, C: Future<Output = Result<I, E>>>(&mut self, create_io: C) -> Result<bool, SetupError> {
        let addresses = self.control.auth.get_control_addresses().await?;

        if self.last_control_targets == addresses {
            return Ok(false);
        }

        let new_io = match create_io.await {
            Ok(v) => v,
            Err(error) => return Err(error.into()),
        };
        
        let connected = AddressSelector::new(addresses.clone(), new_io).connect_to_first().await?;
        let updated = self.replace_connection(connected, false).await?;

        self.last_control_targets = addresses;
        Ok(updated)
    }

    pub async fn replace_connection(&mut self, mut connected: ConnectedControl<I>, force: bool) -> Result<bool, SetupError> {
        if !force
            && self.control.conn.pong_latest.client_addr.ip() == connected.pong_latest.client_addr.ip()
            && self.control.conn.pong_latest.tunnel_addr == connected.pong_latest.tunnel_addr {
            return Ok(false);
        }

        let registered = connected.authenticate(&self.control.auth).await?;

        tracing::info!(old = %self.control.conn.pong_latest.tunnel_addr, new = %connected.pong_latest.tunnel_addr, "update control address");
        connected.reset_established(&mut self.control, registered);

        Ok(true)
    }

    pub async fn send_udp_session_auth(&mut self, now_ms: u64, min_wait_ms: u64) -> bool {
        if now_ms < self.last_udp_auth + min_wait_ms {
            return false;
        }
        
        self.last_udp_auth = now_ms;
        if let Err(error) = self.control.send_setup_udp_channel(1).await {
            tracing::error!(?error, "failed to send setup udp channel request");
        }

        true
    }

    pub async fn update(&mut self) -> Option<TunnelControlEvent> {
        if let Some(reason) = self.control.is_expired() {
            tracing::warn!(?reason, "session expired");

            if let Err(error) = self.control.authenticate().await {
                tracing::error!(?error, "failed to authenticate");
                tokio::time::sleep(Duration::from_secs(2)).await;
                return None;
            }
        }

        let now = now_milli();
        if now - self.last_ping > 1_000 {
            self.last_ping = now;

            if let Err(error) = self.control.send_ping(200, now).await {
                tracing::error!(?error, "failed to send ping");
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

            tracing::info!(time_till_expire, "send KeepAlive");
            if let Err(error) = self.control.send_keep_alive(100).await {
                tracing::error!(?error, "failed to send KeepAlive");
            }
        }

        let mut timeouts = 0;

        for _ in 0..30 {
            match tokio::time::timeout(Duration::from_millis(100), self.control.recv_feed_msg()).await {
                Ok(Ok(ControlFeed::NewClient(new_client))) => return Some(TunnelControlEvent::NewClient(new_client)),
                Ok(Ok(ControlFeed::NewClientOld(new_client))) => return Some(TunnelControlEvent::NewClient(new_client.into())),
                Ok(Ok(ControlFeed::Response(msg))) => match msg.content {
                    ControlResponse::UdpChannelDetails(details) => return Some(TunnelControlEvent::UdpChannelDetails(details)),
                    ControlResponse::Unauthorized => {
                        tracing::info!("session no longer authorized");
                        self.control.set_expired();
                    }
                    ControlResponse::Pong(pong) => {
                        self.last_pong = now_milli();

                        if pong.client_addr != self.control.pong_at_auth.client_addr {
                            tracing::info!(
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
                    tracing::error!(?error, "failed to parse response");
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
            tracing::info!("timeout waiting for pong");

            self.last_pong = 0;
            self.control.set_expired();
        }

        None
    }
}

pub enum TunnelControlEvent {
    NewClient(NewClient),
    UdpChannelDetails(UdpChannelDetails),
}
