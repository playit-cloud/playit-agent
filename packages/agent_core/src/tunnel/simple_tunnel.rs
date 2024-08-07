use std::net::SocketAddr;
use std::time::Duration;

use playit_agent_proto::control_feed::{ControlFeed, NewClient};
use playit_agent_proto::control_messages::ControlResponse;
use tokio::net::UdpSocket;

use crate::api::api::ReqAgentsRoutingGet;
use crate::api::PlayitApi;
use crate::tunnel::control::AuthenticatedControl;
use crate::tunnel::setup::{ConnectedControl, SetupError, SetupFindSuitableChannel};
use crate::tunnel::udp_tunnel::UdpTunnel;
use crate::utils::error_helper::ErrorHelper;
use crate::utils::now_milli;

use super::setup::AuthApi;

pub struct SimpleTunnel {
    api_url: String,
    auth: AuthApi,
    control_addr: SocketAddr,
    control_channel: AuthenticatedControl<AuthApi, UdpSocket>,
    udp_tunnel: UdpTunnel,
    last_keep_alive: u64,
    last_ping: u64,
    last_pong: u64,
    last_udp_auth: u64,
    last_control_targets: Vec<SocketAddr>,
}

impl SimpleTunnel {
    pub async fn setup(api_url: String, secret_key: String) -> Result<Self, SetupError> {
        let udp_tunnel = UdpTunnel::new().await?;

        let auth = AuthApi {
            api_url: api_url.clone(),
            secret_key: secret_key.clone(),
        };

        let addresses = get_control_addresses(&auth).await?;
        let setup = SetupFindSuitableChannel::new(addresses.clone()).setup().await?;
        let control_addr = setup.control_addr;

        let control_channel = setup.authenticate(auth.clone()).await?;

        Ok(SimpleTunnel {
            api_url,
            auth,
            control_addr,
            control_channel,
            udp_tunnel,
            last_keep_alive: 0,
            last_ping: 0,
            last_pong: 0,
            last_udp_auth: 0,
            last_control_targets: addresses,
        })
    }

    pub async fn reload_control_addr(&mut self) -> Result<bool, SetupError> {
        let addresses = get_control_addresses(&self.auth).await?;

        if self.last_control_targets == addresses {
            return Ok(false);
        }

        let setup = SetupFindSuitableChannel::new(addresses.clone()).setup().await?;
        let updated = self.update_control_addr(setup).await?;
        self.last_control_targets = addresses;

        Ok(updated)
    }

    pub async fn update_control_addr(&mut self, connected: ConnectedControl<UdpSocket>) -> Result<bool, SetupError> {
        let new_control_addr = connected.control_addr;
        if self.control_addr == new_control_addr {
            return Ok(false);
        }

        let control_channel = connected.authenticate(self.auth.clone()).await?;

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
        if now - self.last_ping > 1_000 {
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

            tracing::info!(time_till_expire, "send KeepAlive");
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

        let mut timeouts = 0;

        for _ in 0..30 {
            match tokio::time::timeout(Duration::from_millis(100), self.control_channel.recv_feed_msg()).await {
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
                    ControlResponse::Pong(pong) => {
                        self.last_pong = now_milli();

                        if pong.client_addr != self.control_channel.conn.pong.client_addr {
                            tracing::info!(
                                new_client = %pong.client_addr,
                                old_client = %self.control_channel.conn.pong.client_addr,
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
            self.control_channel.set_expired();
        }

        None
    }
}

async fn get_control_addresses(auth: &AuthApi) -> Result<Vec<SocketAddr>, SetupError> {
    let api = auth.api_client();
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
