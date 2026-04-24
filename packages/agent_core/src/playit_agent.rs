use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use tokio::sync::mpsc::channel;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
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
    cancel_token: CancellationToken,
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

        let tunnel_packets = Packets::new(1024 * 8);
        let origin_packets = Packets::new(1024 * 8);
        let udp_channel = UdpChannel::new(tunnel_packets)
            .await
            .map_err(SetupError::IoError)?;

        let stats = AgentStats::new();
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
        let udp_session_should_renew = Arc::new(AtomicBool::new(false));

        let tunnel_cancel = cancel_token.child_token();
        let should_renew_udp = udp_session_should_renew.clone();
        let mut tunnel_task = tokio::spawn(async move {
            let mut last_control_addr_check = now_milli();

            loop {
                // Keep the control loop cooperative when updates are continuously ready.
                tokio::select! {
                    _ = tunnel_cancel.cancelled() => break,
                    _ = tokio::task::yield_now() => {}
                }

                if should_renew_udp.load(Ordering::Acquire) {
                    let Some(sent) = tunnel_cancel
                        .run_until_cancelled(control.send_udp_session_auth(now_milli(), 5_000))
                        .await
                    else {
                        break;
                    };
                    if sent {
                        tracing::info!("udp channel requires auth, sent auth request");
                    }
                }

                let now = now_milli();
                if 30_000 < now.saturating_sub(last_control_addr_check) {
                    last_control_addr_check = now;

                    let reload =
                        control.reload_control_addr(async { DualStackUdpSocket::new().await });
                    if let Some(Err(error)) = tunnel_cancel.run_until_cancelled(reload).await {
                        tracing::error!(?error, "failed to reload_control_addr");
                    }
                }

                let update = tokio::select! {
                    _ = tunnel_cancel.cancelled() => break,
                    update = control.update() => update,
                };

                match update {
                    Some(TunnelControlEvent::NewClient(new_client)) => {
                        tokio::select! {
                            _ = tunnel_cancel.cancelled() => break,
                            _ = tcp_clients.handle_new_client(new_client) => {}
                        }
                    }
                    Some(TunnelControlEvent::UdpChannelDetails(udp_details)) => {
                        tracing::info!("udp session details received");
                        let _ = udp_session_tx.try_send(udp_details);
                    }
                    None => {}
                }
            }
        });

        let udp_cancel = cancel_token.child_token();
        let mut udp_channel = udp_channel;
        let mut udp_clients = udp_clients;

        let mut udp_task = tokio::spawn(async move {
            let mut next_clear = Instant::now() + Duration::from_secs(16);

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
                            tracing::warn!("udp session channel closed");
                            break;
                        };
                        udp_channel.update_session(session).await;
                    }
                    _ = tokio::time::sleep_until(next_clear) => {
                        next_clear = Instant::now() + Duration::from_secs(16);
                        udp_clients.clear_old(now_milli()).await;
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

        if !tunnel_done {
            if tokio::time::timeout(Duration::from_secs(5), &mut tunnel_task)
                .await
                .is_err()
            {
                tunnel_task.abort();
                let _ = tunnel_task.await;
            }
        }
        if !udp_done {
            if tokio::time::timeout(Duration::from_secs(5), &mut udp_task)
                .await
                .is_err()
            {
                udp_task.abort();
                let _ = udp_task.await;
            }
        }
    }
}
