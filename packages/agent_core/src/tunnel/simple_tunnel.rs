use std::net::SocketAddr;
use std::time::Duration;

use playit_agent_proto::control_feed::{ControlFeed, NewClient};
use playit_agent_proto::control_messages::ControlResponse;

use crate::api::api::ReqAgentsRoutingGet;
use crate::api::PlayitApi;
use crate::tunnel::control::AuthenticatedControl;
use crate::tunnel::setup::{ConnectedControl, SetupError, SetupFindSuitableChannel};
use crate::tunnel::udp_tunnel::UdpTunnel;
use crate::utils::error_helper::ErrorHelper;
use crate::utils::now_milli;

pub struct SimpleTunnel {
    api_url: String,
    secret_key: String,
    control_addr: SocketAddr,
    control_channel: AuthenticatedControl,
    udp_tunnel: UdpTunnel,
    last_keep_alive: u64,
    last_ping: u64,
    last_udp_auth: u64,
    last_control_targets: Vec<SocketAddr>,
}

impl SimpleTunnel {
    pub async fn setup(api_url: String, secret_key: String) -> Result<Self, SetupError> {
        let udp_tunnel = UdpTunnel::new().await?;

        let addresses = get_control_addresses(api_url.clone(), secret_key.clone()).await?;
        let setup = SetupFindSuitableChannel::new(addresses.clone()).setup().await?;
        let control_addr = setup.control_addr;
        let control_channel = setup.authenticate(api_url.clone(), secret_key.clone()).await?;

        Ok(SimpleTunnel {
            api_url,
            secret_key,
            control_addr,
            control_channel,
            udp_tunnel,
            last_keep_alive: 0,
            last_ping: 0,
            last_udp_auth: 0,
            last_control_targets: addresses,
        })
    }

    pub async fn reload_control_addr(&mut self) -> Result<bool, SetupError> {
        let addresses = get_control_addresses(self.api_url.clone(), self.secret_key.clone()).await?;

        if self.last_control_targets == addresses {
            return Ok(false);
        }

        let setup = SetupFindSuitableChannel::new(addresses.clone()).setup().await?;
        let updated = self.update_control_addr(setup).await?;
        self.last_control_targets = addresses;

        Ok(updated)
    }

    pub async fn update_control_addr(&mut self, connected: ConnectedControl) -> Result<bool, SetupError> {
        let new_control_addr = connected.control_addr;
        if self.control_addr == new_control_addr {
            return Ok(false);
        }

        let control_channel = connected.authenticate(self.api_url.clone(), self.secret_key.clone()).await?;

        tracing::info!(old = %self.control_addr, new = %new_control_addr, "update control address");
        self.control_channel = control_channel;
        self.control_addr = new_control_addr;
        self.last_ping = 0;
        self.last_keep_alive = 0;
        self.last_udp_auth = 0;

        self.udp_tunnel.invalidate_session();
        Ok(true)
    }

    pub fn udp_tunnel(&self) -> UdpTunnel {
        self.udp_tunnel.clone()
    }

    pub async fn update(&mut self) -> Option<NewClient> {
        if self.control_channel.is_expired() {
            if let Err(error) = self.control_channel.authenticate().await {
                tracing::error!(?error, "failed to authenticate");
                tokio::time::sleep(Duration::from_secs(2)).await;
                return None;
            }
        }

        let now = now_milli();
        if now - self.last_ping > 5_000 {
            self.last_ping = now;

            if let Err(error) = self.control_channel.send_ping(200, now).await {
                tracing::error!(?error, "failed to send ping");
            }
        }

        if self.udp_tunnel.requires_auth() {
            if 5_000 < now - self.last_udp_auth {
                self.last_udp_auth = now;

                if let Err(error) = self.control_channel.send_setup_udp_channel(9000).await {
                    tracing::error!(?error, "failed to send udp setup request to control");
                }
            }
        } else if self.udp_tunnel.requires_resend() {
            if 1_000 < now - self.last_udp_auth {
                self.last_udp_auth = now;

                if let Err(error) = self.udp_tunnel.resend_token().await {
                    tracing::error!(?error, "failed to send udp auth request");
                }
            }
        }

        let time_till_expire = self.control_channel.get_expire_at().max(now) - now;
        tracing::trace!(time_till_expire, "time till expire");

        /* 30 seconds till expiry and haven't sent in last 10 sec */
        if 10_000 < now - self.last_keep_alive && time_till_expire < 30_000 {
            self.last_keep_alive = now;

            tracing::debug!("sent KeepAlive");
            if let Err(error) = self.control_channel.send_keep_alive(100).await {
                tracing::error!(?error, "failed to send KeepAlive");
            }

            self.control_channel
                .send_setup_udp_channel(1)
                .await
                .take_error(|error| {
                    tracing::error!(?error, "failed to send setup udp channel request");
                });
        }

        match tokio::time::timeout(Duration::from_secs(1), self.control_channel.recv_feed_msg()).await {
            Ok(Ok(ControlFeed::NewClient(new_client))) => return Some(new_client),
            Ok(Ok(ControlFeed::Response(msg))) => match msg.content {
                ControlResponse::UdpChannelDetails(details) => {
                    tracing::info!(?details, "update udp channel details");
                    self.udp_tunnel.set_udp_tunnel(details).await.unwrap();
                }
                ControlResponse::Unauthorized => {
                    tracing::info!("session no longer authorized");
                    self.control_channel.set_expired();
                }
                msg => {
                    tracing::debug!(?msg, "got response");
                }
            },
            Ok(Err(error)) => {
                tracing::error!(?error, "failed to parse response");
            }
            Err(_) => {
                tracing::trace!("feed recv timeout");
            }
        }

        None
    }
}

async fn get_control_addresses(api_url: String, secret_key: String) -> Result<Vec<SocketAddr>, SetupError> {
    let api = PlayitApi::create(api_url, Some(secret_key));
    let routing = api.agents_routing_get(ReqAgentsRoutingGet { agent_id: None }).await?;

    let mut addresses = vec![];
    for ip6 in routing.targets6 {
        addresses.push(SocketAddr::new(ip6.into(), 5525));
    }
    for ip4 in routing.targets4 {
        addresses.push(SocketAddr::new(ip4.into(), 5525));
    }

    tracing::info!("control address {:?}", addresses);

    Ok(addresses)
}
