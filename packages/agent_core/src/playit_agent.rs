use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::channel;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use playit_api_client::api::{AgentVersion, Platform};

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

const CONTROL_ADDRESS_RELOAD_INTERVAL: Duration = Duration::from_secs(30);
const UDP_CLIENT_CLEAR_INTERVAL: Duration = Duration::from_secs(16);
const UDP_SESSION_RENEW_AFTER: Duration = Duration::from_secs(6);
const UDP_SESSION_CHECK_INTERVAL: Duration = Duration::from_secs(3);
const TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const UDP_SESSION_AUTH_TTL_MILLIS: u64 = 5_000;

pub struct PlayitAgent {
    control: MaintainedControl<DualStackUdpSocket, AuthApi>,

    udp_clients: UdpClients,
    udp_channel: UdpChannel,

    tcp_clients: TcpClients,
    cancel_token: CancellationToken,
    stats: AgentStats,
}

#[derive(Clone, Debug)]
pub struct PlayitAgentSettings {
    pub api_url: String,
    pub secret_key: String,
    pub agent_version: AgentVersion,
    pub platform: Platform,
    pub tcp_settings: TcpSettings,
    pub udp_settings: UdpSettings,
}

impl PlayitAgent {
    pub async fn new(
        settings: PlayitAgentSettings,
        lookup: Arc<OriginLookup>,
    ) -> Result<Self, SetupError> {
        let io = DualStackUdpSocket::new().await?;
        let auth = AuthApi::new(
            settings.api_url,
            settings.secret_key,
            settings.agent_version,
            settings.platform,
        );
        let control = MaintainedControl::setup(io, auth).await?;

        let tunnel_packets = Packets::new(1024 * 8);
        let origin_packets = Packets::new(1024 * 8);
        let stats = AgentStats::new();
        let udp_channel = UdpChannel::new(tunnel_packets, stats.clone())
            .await
            .map_err(SetupError::IoError)?;

        let udp_clients = UdpClients::new(
            settings.udp_settings,
            lookup.clone(),
            origin_packets,
            stats.clone(),
        );
        let cancel_token = CancellationToken::new();
        let tcp_clients = TcpClients::new(
            settings.tcp_settings,
            lookup.clone(),
            stats.clone(),
            cancel_token.child_token(),
        );

        Ok(PlayitAgent {
            control,
            udp_clients,
            udp_channel,
            tcp_clients,
            cancel_token,
            stats,
        })
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Get a handle to the agent stats
    pub fn stats(&self) -> AgentStats {
        self.stats.clone()
    }

    pub async fn run(self) {
        let PlayitAgent {
            mut control,
            udp_clients,
            udp_channel,
            tcp_clients,
            cancel_token,
            ..
        } = self;

        let (udp_session_tx, mut udp_session_rx) = channel(8);
        let (udp_renew_tx, mut udp_renew_rx) = channel(1);

        let tunnel_cancel = cancel_token.child_token();
        let mut tunnel_task = tokio::spawn(async move {
            let mut control_address_reload = tokio::time::interval_at(
                Instant::now() + CONTROL_ADDRESS_RELOAD_INTERVAL,
                CONTROL_ADDRESS_RELOAD_INTERVAL,
            );

            loop {
                tokio::select! {
                    _ = tunnel_cancel.cancelled() => break,
                    renew = udp_renew_rx.recv() => {
                        if renew.is_none() {
                            tracing::debug!("udp renewal channel closed");
                            break;
                        }
                        let Some(sent) = tunnel_cancel
                            .run_until_cancelled(control.send_udp_session_auth(
                                now_milli(),
                                UDP_SESSION_AUTH_TTL_MILLIS,
                            ))
                            .await
                        else {
                            break;
                        };
                        if sent {
                            tracing::debug!("udp channel requires auth, sent auth request");
                        }
                    }
                    _ = control_address_reload.tick() => {
                        let reload =
                            control.reload_control_addr(async { DualStackUdpSocket::new().await });
                        if let Some(Err(error)) = tunnel_cancel.run_until_cancelled(reload).await {
                            tracing::error!(?error, "failed to reload_control_addr");
                        }
                    }
                    update = control.update() => match update {
                        Some(TunnelControlEvent::NewClient(new_client)) => {
                            tokio::select! {
                                _ = tunnel_cancel.cancelled() => break,
                                _ = tcp_clients.handle_new_client(new_client) => {}
                            }
                        }
                        Some(TunnelControlEvent::UdpChannelDetails(udp_details)) => {
                            tracing::debug!("udp session details received");
                            let _ = udp_session_tx.try_send(udp_details);
                        }
                        None => {}
                    },
                }
            }
        });

        let udp_cancel = cancel_token.child_token();
        let mut udp_channel = udp_channel;
        let mut udp_clients = udp_clients;

        let mut udp_task = tokio::spawn(async move {
            let mut next_clear = Instant::now() + UDP_CLIENT_CLEAR_INTERVAL;

            loop {
                // Keep the UDP packet loop cooperative under sustained bidirectional traffic.
                tokio::select! {
                    _ = udp_cancel.cancelled() => break,
                    _ = tokio::task::yield_now() => {}
                }

                tokio::select! {
                    _ = udp_cancel.cancelled() => break,
                    recv = udp_clients.recv_origin_packet() => {
                        let Some((flow, packet)) = udp_clients.dispatch_origin_packet(now_milli(), recv).await else { continue };
                        udp_channel.send(flow, packet).await;
                    }
                    (flow, packet) = udp_channel.recv() => {
                        udp_clients.handle_tunneled_packet(now_milli(), flow, packet).await;
                    }
                    session_opt = udp_session_rx.recv() => {
                        let Some(session) = session_opt else {
                            tracing::debug!("udp session channel closed");
                            break;
                        };
                        udp_channel.update_session(session).await;
                    }
                    _ = tokio::time::sleep_until(next_clear) => {
                        next_clear = Instant::now() + UDP_CLIENT_CLEAR_INTERVAL;
                        udp_clients.clear_old(now_milli()).await;
                    }
                    _ = tokio::time::sleep(UDP_SESSION_CHECK_INTERVAL) => {}
                }

                let udp_needs_renew = match udp_channel.time_since_established() {
                    Some(since) => UDP_SESSION_RENEW_AFTER <= since,
                    None => true,
                };
                if udp_needs_renew {
                    let _ = udp_renew_tx.try_send(());
                }
            }
        }.instrument(tracing::info_span!("udp_session")));

        let mut tunnel_done = false;
        let mut udp_done = false;
        tokio::select! {
            result = &mut tunnel_task => {
                tunnel_done = true;
                if let Err(error) = result {
                    tracing::error!(?error, "tunnel task failed");
                }
            }
            result = &mut udp_task => {
                udp_done = true;
                if let Err(error) = result {
                    tracing::error!(?error, "udp task failed");
                }
            }
            _ = cancel_token.cancelled() => {}
        }

        cancel_token.cancel();

        shutdown_task("tunnel", tunnel_done, &mut tunnel_task).await;
        shutdown_task("udp", udp_done, &mut udp_task).await;
    }
}

async fn shutdown_task(name: &'static str, completed: bool, task: &mut JoinHandle<()>) {
    if completed {
        return;
    }

    match tokio::time::timeout(TASK_SHUTDOWN_TIMEOUT, &mut *task).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!(task = name, ?error, "agent task failed"),
        Err(_) => {
            tracing::warn!(task = name, "agent task did not stop in time; aborting");
            task.abort();
            let _ = task.await;
        }
    }
}
