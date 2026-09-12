use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

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
        let Self {
            mut control,
            mut udp_clients,
            mut udp_channel,
            tcp_clients,
            cancel_token,
            ..
        } = self;
        let (session_tx, mut session_rx) = watch::channel(None);
        let (renew_tx, renew_rx) = watch::channel(true);

        let control_loop = async move {
            let mut next_address_check = Instant::now() + Duration::from_secs(30);
            loop {
                if *renew_rx.borrow() {
                    control.send_udp_session_auth(Duration::from_secs(5)).await;
                }
                if Instant::now() >= next_address_check {
                    next_address_check = Instant::now() + Duration::from_secs(30);
                    if let Err(error) = control.reload_control_addr(DualStackUdpSocket::new()).await
                    {
                        tracing::debug!(?error, "Control address refresh failed");
                    }
                }
                match control.update().await {
                    Some(TunnelControlEvent::NewClient(client)) => {
                        tcp_clients.handle_new_client(client).await
                    }
                    Some(TunnelControlEvent::UdpChannelDetails(details)) => {
                        session_tx.send_replace(Some(details));
                    }
                    None => {}
                }
            }
        };
        let udp_loop = async move {
            let mut maintenance = tokio::time::interval(Duration::from_secs(3));
            maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    packet = udp_clients.recv_origin_packet() => {
                        if let Some((flow, packet)) = udp_clients.dispatch_origin_packet(packet).await
                            && udp_channel.send(flow, packet).await.is_err() { break; }
                    }
                    packet = udp_channel.recv() => {
                        let Some((flow, packet)) = packet else { break };
                        udp_clients.handle_tunneled_packet(flow, packet).await;
                    }
                    result = session_rx.changed() => {
                        if result.is_err() { break; }
                        let session = session_rx.borrow_and_update().clone();
                        if let Some(session) = session
                            && udp_channel.update_session(session).await.is_err() { break; }
                    }
                    _ = maintenance.tick() => {
                        udp_clients.clear_old().await;
                        let renew = udp_channel.time_since_established()
                            .is_none_or(|age| age >= Duration::from_secs(6));
                        renew_tx.send_replace(renew);
                    }
                }
            }
        };
        // Dropping either loop also drops its transports and their owned workers.
        tokio::select! {
            _ = cancel_token.cancelled() => {}
            _ = control_loop => {}
            _ = udp_loop => {}
        }
        cancel_token.cancel();
    }
}
