use std::time::Duration;

use playit_agent_proto::control_feed::{ControlFeed, NewClient};
use playit_agent_proto::control_messages::ControlResponse;

use crate::tunnel::control_channel::ControlConnected;
use crate::tunnel::setup::{SetupError, SetupFindSuitableChannel};
use crate::tunnel::udp_tunnel::UdpTunnel;
use crate::utils::error_helper::ErrorHelper;
use crate::utils::name_lookup::address_lookup;
use crate::utils::now_milli;

pub struct SimpleTunnel {
    control_channel: ControlConnected,
    udp_tunnel: UdpTunnel,
    last_keep_alive: u64,
    last_ping: u64,
}

impl SimpleTunnel {
    pub async fn setup(secret_key: String) -> Result<Self, SetupError> {
        let addresses = address_lookup("control.playit.gg", 5525).await;
        let setup = SetupFindSuitableChannel::new(addresses).setup().await?;
        let mut control_channel = setup.authenticate(secret_key).await?;

        Ok(SimpleTunnel {
            control_channel,
            udp_tunnel: Default::default(),
            last_keep_alive: 0,
            last_ping: 0,
        })
    }

    pub fn udp_tunnel(&self) -> UdpTunnel {
        self.udp_tunnel.clone()
    }

    pub async fn update(&mut self) -> Option<NewClient> {
        let now = now_milli();

        if now - self.last_ping > 5_000 {
            self.last_ping = now;
            if let Err(error) = self.control_channel.send_ping(200, now).await {
                tracing::error!(?error, "failed to send ping");
            }

            if !self.udp_tunnel.is_setup().await {
                self.control_channel.send_setup_udp_channel(1).await.take_error(|error| {
                    tracing::error!(?error, "failed to send setup udp channel request");
                });
            }
        }

        let time_till_expire = self.control_channel.get_expire().max(now) - now;
        tracing::info!(time_till_expire, "time till expire");

        /* 30 seconds till expiry and haven't sent in last 10 sec */
        if 10_000 < now - self.last_keep_alive && time_till_expire < 30_000 {
            self.last_keep_alive = now;

            tracing::info!("sent KeepAlive");
            if let Err(error) = self.control_channel.send_keep_alive(100).await {
                tracing::error!(?error, "failed to send KeepAlive");
            }
        }

        match tokio::time::timeout(Duration::from_secs(10), self.control_channel.recv_feed_msg()).await {
            Ok(Ok(ControlFeed::NewClient(new_client))) => return Some(new_client),
            Ok(Ok(ControlFeed::Response(msg))) => {
                match msg.content {
                    ControlResponse::UdpChannelDetails(details) => {
                        self.udp_tunnel.set_udp_tunnel(details).await.unwrap();
                    }
                    msg => {
                        tracing::info!(?msg, "got response");
                    }
                }
            }
            Ok(Err(error)) => {
                tracing::error!(?error, "failed to parse response");
            }
            Err(_) => {}
        }

        None
    }
}