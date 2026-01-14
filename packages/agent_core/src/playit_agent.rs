use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::channel;
use tokio::time::Instant;
use tracing::Instrument;

use crate::agent_control::errors::SetupError;
use crate::agent_control::maintained_control::{MaintainedControl, TunnelControlEvent};
use crate::agent_control::{AuthApi, DualStackUdpSocket};
use crate::network::origin_lookup::OriginLookup;
use crate::network::tcp::tcp_clients::TcpClients;
use crate::network::tcp::tcp_settings::TcpSettings;
use crate::network::udp::packets::Packets;
use crate::network::udp::udp_channel::UdpChannel;
use crate::network::udp::udp_clients::UdpClients;
use crate::network::udp::udp_settings::UdpSettings;
use crate::stats::AgentStats;
use crate::utils::now_milli;

pub struct PlayitAgent {
    control: MaintainedControl<DualStackUdpSocket, AuthApi>,

    udp_clients: UdpClients,
    udp_channel: UdpChannel,

    tcp_clients: TcpClients,
    keep_running: Arc<AtomicBool>,
    stats: AgentStats,
}

#[derive(Clone, Debug)]
pub struct PlayitAgentSettings {
    pub api_url: String,
    pub secret_key: String,
    pub tcp_settings: TcpSettings,
    pub udp_settings: UdpSettings,
}

impl PlayitAgent {
    pub async fn new(
        settings: PlayitAgentSettings,
        lookup: Arc<OriginLookup>,
    ) -> Result<Self, SetupError> {
        let io = DualStackUdpSocket::new().await?;
        let auth = AuthApi::new(settings.api_url, settings.secret_key);
        let control = MaintainedControl::setup(io, auth).await?;

        let packets = Packets::new(1024 * 16);
        let udp_channel = UdpChannel::new(packets.clone())
            .await
            .map_err(SetupError::IoError)?;

        let stats = AgentStats::new();
        let udp_clients = UdpClients::new(settings.udp_settings, lookup.clone(), packets.clone(), stats.clone());
        let tcp_clients = TcpClients::new(settings.tcp_settings, lookup.clone(), stats.clone());

        Ok(PlayitAgent {
            control,
            udp_clients,
            udp_channel,
            tcp_clients,
            keep_running: Arc::new(AtomicBool::new(true)),
            stats,
        })
    }

    pub fn keep_running(&self) -> Arc<AtomicBool> {
        self.keep_running.clone()
    }

    /// Get a handle to the agent stats
    pub fn stats(&self) -> AgentStats {
        self.stats.clone()
    }

    pub async fn run(self) {
        let mut control = self.control;
        let tunnel_run = self.keep_running.clone();

        let (udp_session_tx, mut udp_session_rx) = channel(8);
        let udp_session_should_renew = Arc::new(AtomicBool::new(false));

        let should_renew_udp = udp_session_should_renew.clone();
        let tunnel_task = tokio::spawn(async move {
            let mut last_control_addr_check = now_milli();

            while tunnel_run.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;

                if should_renew_udp.load(Ordering::Acquire)
                    && control.send_udp_session_auth(now_milli(), 5_000).await
                {
                    tracing::info!("udp channel requires auth, sent auth request");
                }

                /* refresh control address every 30s */
                {
                    let now = now_milli();
                    if 30_000 < now_milli() - last_control_addr_check {
                        last_control_addr_check = now;

                        if let Err(error) = control
                            .reload_control_addr(async { DualStackUdpSocket::new().await })
                            .await
                        {
                            tracing::error!(?error, "failed to reload_control_addr");
                        }
                    }
                }

                match control.update().await {
                    Some(TunnelControlEvent::NewClient(new_client)) => {
                        self.tcp_clients.handle_new_client(new_client).await;
                    }
                    Some(TunnelControlEvent::UdpChannelDetails(udp_details)) => {
                        tracing::info!("udp session details received");
                        let _ = udp_session_tx.try_send(udp_details);
                    }
                    None => {}
                }
            }
        });

        let udp_run = self.keep_running.clone();

        let mut udp_channel = self.udp_channel;
        let mut udp_clients = self.udp_clients;

        let udp_task = tokio::spawn(async move {
            let mut next_clear = Instant::now() + Duration::from_secs(16);

            while udp_run.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;

                tokio::select! {
                    recv = udp_clients.recv_origin_packet() => {
                        let Some((flow, packet)) = udp_clients.dispatch_origin_packet(now_milli(), recv).await else { continue };
                        udp_channel.send(flow, packet).await;
                    }
                    (flow, packet) = udp_channel.recv() => {
                        udp_clients.handle_tunneled_packet(now_milli(), flow, packet).await;
                    }
                    session_opt = udp_session_rx.recv() => {
                        let Some(session) = session_opt else {
                            tracing::warn!("udp session channel closed");
                            break;
                        };
                        udp_channel.update_session(session).await;
                    }
                    _ = tokio::time::sleep_until(next_clear) => {
                        next_clear = Instant::now() + Duration::from_secs(16);
                        udp_clients.clear_old(now_milli());
                    }
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }

                {
                    let udp_needs_renew = match udp_channel.time_since_established() {
                        Some(since) => Duration::from_secs(6) <= since,
                        None => true,
                    };
                    udp_session_should_renew.store(udp_needs_renew, Ordering::Release);
                }
            }
        }.instrument(tracing::info_span!("udp_session")));

        tunnel_task.await.unwrap();
        udp_task.await.unwrap();
    }
}
