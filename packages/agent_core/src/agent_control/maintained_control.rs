use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use playit_agent_proto::control_feed::{ControlFeed, NewClient};
use playit_agent_proto::control_messages::{ControlResponse, ExperimentResultEntry, ExperimentResults, MAX_EXPERIMENT_ENTRIES};

use crate::agent_control::established_control::EstablishedControl;
use crate::agent_control::udp_channel::UdpChannel;
use crate::api::api::AgentRoutingTarget;
use crate::utils::error_helper::ErrorHelper;
use crate::utils::now_milli;

use super::address_selector::AddressSelector;
use super::connected_control::ConnectedControl;
use super::errors::SetupError;
use super::{AuthResource, PacketIO};


pub struct MaintainedControl<I: PacketIO, A: AuthResource> {
    control: EstablishedControl<A, I>,
    udp: Option<UdpChannel<I>>,
    last_keep_alive: u64,
    last_ping: u64,
    last_pong: u64,
    last_udp_auth: u64,
    last_control_targets: Vec<SocketAddr>,
    experiment_results: HashMap<u64, ExperimentResultEntry>,
}

impl<I: PacketIO, A: AuthResource> MaintainedControl<I, A> {
    pub async fn setup(io: I, auth: A, addresses: Vec<SocketAddr>, udp: Option<UdpChannel<I>>) -> Result<Self, SetupError> {
        let setup = AddressSelector::new(addresses.clone(), io).connect_to_first().await?;
        let control_channel: EstablishedControl<A, I> = setup.auth_into_established(auth).await?;

        Ok(MaintainedControl {
            control: control_channel,
            udp,
            last_keep_alive: 0,
            last_ping: 0,
            last_pong: 0,
            last_udp_auth: 0,
            last_control_targets: addresses,
            experiment_results: HashMap::new(),
        })
    }

    pub async fn reload_control_addr<E: Into<SetupError>, C: Future<Output = Result<I, E>>>(&mut self, create_io: C) -> Result<Option<AgentRoutingTarget>, SetupError> {
        let (target, addresses) = self.control.auth.get_control_addresses().await?;

        if self.last_control_targets == addresses {
            return Ok(None);
        }

        let new_io = match create_io.await {
            Ok(v) => v,
            Err(error) => return Err(error.into()),
        };
        
        let connected = AddressSelector::new(addresses.clone(), new_io).connect_to_first().await?;
        let updated = self.replace_connection(connected, false).await?;

        self.last_control_targets = addresses;
        Ok(updated.then_some(target))
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

        if let Some(udp) = &self.udp {
            udp.invalidate_session();
        }

        Ok(true)
    }

    pub fn udp_channel(&self) -> Option<UdpChannel<I>> {
        self.udp.clone()
    }

    pub fn queue_ping_experiment_entry(&mut self, entry: ExperimentResultEntry) {
        self.experiment_results.insert(entry.id, entry);
    }

    pub async fn update(&mut self) -> Option<NewClient> {
        if self.control.is_expired() {
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

            if !self.experiment_results.is_empty() {
                self.control.send_experiment_results(
                    now,
                    ExperimentResults {
                        results: self.experiment_results.drain()
                            .map(|(_, value)| value)
                            .take(MAX_EXPERIMENT_ENTRIES)
                            .collect(),
                    }
                ).await;
            }
        }

        if let Some(udp) = &self.udp {
            if udp.requires_auth() {
                if 5_000 < now - self.last_udp_auth {
                    self.last_udp_auth = now;
    
                    if let Err(error) = self.control.send_setup_udp_channel(9000).await {
                        tracing::error!(?error, "failed to send udp setup request to control");
                    }
                }
            } else if udp.requires_resend() {
                if 1_000 < now - self.last_udp_auth {
                    self.last_udp_auth = now;
    
                    if let Err(error) = udp.resend_token().await {
                        tracing::error!(?error, "failed to send udp auth request");
                    }
                }
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

            self.control
                .send_setup_udp_channel(1)
                .await
                .take_error(|error| {
                    tracing::error!(?error, "failed to send setup udp channel request");
                });
        }

        let mut timeouts = 0;

        for _ in 0..30 {
            match tokio::time::timeout(Duration::from_millis(100), self.control.recv_feed_msg()).await {
                Ok(Ok(ControlFeed::NewClient(new_client))) => return Some(new_client),
                Ok(Ok(ControlFeed::Response(msg))) => match msg.content {
                    ControlResponse::UdpChannelDetails(details) => {
                        tracing::info!(?details, "update udp channel details");
                        if let Some(udp) = &self.udp {
                            udp.set_udp_tunnel(details).await.unwrap();
                        }
                    }
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
