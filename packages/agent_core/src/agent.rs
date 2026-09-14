use std::{sync::Arc, time::Duration};

use playit_api_client::{
    PlayitApi,
    api::{AgentVersion, Platform},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    control::{ControlChannel, ControlEvent, ControlSettings, PlayitControlApi},
    error::SetupError,
    origin::OriginLookup,
    platform::{crate_agent_version, current_platform},
    stats::AgentStats,
    tcp::{TcpSettings, TcpTunnels},
    udp::{UdpSettings, UdpTunnel},
};

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub api_url: String,
    pub secret_key: String,
    pub version: AgentVersion,
    pub platform: Platform,
    pub control: ControlSettings,
    pub tcp: TcpSettings,
    pub udp: UdpSettings,
}

impl AgentConfig {
    pub fn new(api_url: impl Into<String>, secret_key: impl Into<String>) -> Self {
        AgentConfig {
            api_url: api_url.into(),
            secret_key: secret_key.into(),
            version: crate_agent_version(),
            platform: current_platform(),
            control: ControlSettings::default(),
            tcp: TcpSettings::default(),
            udp: UdpSettings::default(),
        }
    }
}

/// A connected agent: control channel plus TCP and UDP tunnel handlers.
pub struct PlayitAgent {
    control: ControlChannel<PlayitControlApi>,
    tcp: TcpTunnels,
    udp: UdpTunnel,
    stats: AgentStats,
    cancel: CancellationToken,
}

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

impl PlayitAgent {
    /// Registers with the tunnel server. Fails if the API rejects the secret or no
    /// control server answers.
    pub async fn connect(
        config: AgentConfig,
        lookup: Arc<OriginLookup>,
    ) -> Result<Self, SetupError> {
        let client = PlayitApi::create(config.api_url, Some(config.secret_key));
        let api = PlayitControlApi::new(client, config.version, config.platform);
        let control = ControlChannel::connect(api, config.control).await?;

        let stats = AgentStats::new();
        let cancel = CancellationToken::new();
        let tcp = TcpTunnels::new(
            config.tcp,
            lookup.clone(),
            stats.clone(),
            cancel.child_token(),
        );
        let udp = UdpTunnel::new(config.udp, lookup, stats.clone(), control.handle()).await?;

        Ok(PlayitAgent {
            control,
            tcp,
            udp,
            stats,
            cancel,
        })
    }

    pub fn stats(&self) -> AgentStats {
        self.stats.clone()
    }

    /// Cancelling this token stops [`PlayitAgent::run`].
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn tcp(&self) -> &TcpTunnels {
        &self.tcp
    }

    pub fn control(&self) -> &ControlChannel<PlayitControlApi> {
        &self.control
    }

    /// Runs until the cancellation token fires.
    pub async fn run(self) {
        let PlayitAgent {
            mut control,
            tcp,
            udp,
            cancel,
            ..
        } = self;

        let (session_tx, session_rx) = mpsc::channel(8);
        let mut udp_task = tokio::spawn(udp.run(session_rx, cancel.child_token()));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                event = control.next_event() => match event {
                    ControlEvent::NewClient(client) => tcp.handle_new_client(client),
                    ControlEvent::UdpSession(details) => {
                        if session_tx.try_send(details).is_err() {
                            tracing::debug!("udp session update dropped, tunnel task busy");
                        }
                    }
                },
                result = &mut udp_task => {
                    if let Err(error) = result {
                        tracing::error!(?error, "udp tunnel task failed");
                    }
                    break;
                }
            }
        }

        cancel.cancel();
        if !udp_task.is_finished()
            && tokio::time::timeout(SHUTDOWN_GRACE, &mut udp_task)
                .await
                .is_err()
        {
            udp_task.abort();
        }
    }
}
