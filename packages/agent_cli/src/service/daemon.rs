//! Daemon entry point for running the agent as a background service.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use playit_agent_core::agent_control::platform::current_platform;
use playit_api_client::api::Platform;
use playit_agent_core::network::origin_lookup::{OriginLookup, OriginResource, OriginTarget};
use playit_agent_core::network::tcp::tcp_settings::TcpSettings;
use playit_agent_core::network::udp::udp_settings::UdpSettings;
use playit_agent_core::playit_agent::{PlayitAgent, PlayitAgentSettings};
use playit_agent_core::stats::AgentStats;
use playit_agent_core::utils::now_milli;
use playit_api_client::api::AccountStatus;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::playit_secret::PlayitSecret;
use crate::service::ipc::{IpcError, IpcServer, ServiceEvent};
use crate::ui::log_capture::IpcBroadcastLayer;
use crate::ui::tui_app::{
    AccountStatusInfo, AgentData, NoticeInfo, PendingTunnelInfo, TunnelInfo,
};
use crate::API_BASE;

/// Error type for daemon operations
#[derive(Debug)]
pub enum DaemonError {
    Ipc(IpcError),
    SecretError(String),
    SetupError(String),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::Ipc(e) => write!(f, "IPC error: {}", e),
            DaemonError::SecretError(e) => write!(f, "Secret error: {}", e),
            DaemonError::SetupError(e) => write!(f, "Setup error: {}", e),
        }
    }
}

impl std::error::Error for DaemonError {}

impl From<IpcError> for DaemonError {
    fn from(e: IpcError) -> Self {
        DaemonError::Ipc(e)
    }
}

/// Run the daemon (background service)
pub async fn run_daemon(system_mode: bool) -> Result<(), DaemonError> {
    let start_time = now_milli();

    // Create broadcast channel for IPC events (including logs)
    let (event_tx, _) = broadcast::channel::<ServiceEvent>(256);

    // Set up tracing with IPC broadcast layer
    let log_filter =
        EnvFilter::try_from_env("PLAYIT_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    let ipc_log_layer = IpcBroadcastLayer::new(event_tx.clone());

    // Also log to stderr for debugging (with color on Linux)
    let use_ansi = current_platform() == Platform::Linux;

    tracing_subscriber::registry()
        .with(log_filter)
        .with(ipc_log_layer)
        .with(tracing_subscriber::fmt::layer().with_ansi(use_ansi).with_writer(std::io::stderr))
        .init();

    tracing::info!("Starting playit daemon (system_mode={})", system_mode);

    // Shutdown signal
    let cancel_token = CancellationToken::new();

    // Create IPC server (this also enforces single-instance)
    let ipc_server = Arc::new(
        IpcServer::new_with_sender(system_mode, cancel_token.clone(), event_tx.clone())
            .await
            .map_err(DaemonError::Ipc)?,
    );
    let event_tx = ipc_server.event_sender();

    tracing::info!("IPC server created");

    // Load secret
    let mut secret = PlayitSecret::from_args(None, None, false).await;
    let _ = secret.with_default_path().await;

    // Get or wait for valid secret
    let secret_code = match get_secret(&mut secret).await {
        Ok(code) => code,
        Err(e) => {
            tracing::error!("Failed to get secret: {}", e);
            return Err(DaemonError::SecretError(e));
        }
    };

    let api = playit_api_client::PlayitApi::create(API_BASE.to_string(), Some(secret_code.clone()));

    // Setup origin lookup
    let lookup = Arc::new(OriginLookup::default());
    match api.v1_agents_rundata().await {
        Ok(data) => lookup.update_from_run_data(&data).await,
        Err(e) => {
            tracing::warn!("Failed to load initial rundata: {:?}", e);
        }
    }

    // Create agent settings
    let settings = PlayitAgentSettings {
        udp_settings: UdpSettings::default(),
        tcp_settings: TcpSettings::default(),
        api_url: API_BASE.to_string(),
        secret_key: secret_code,
    };

    // Start the agent
    let (runner, stats) = match PlayitAgent::new(settings, lookup.clone()).await {
        Ok(res) => {
            let stats = res.stats();
            (res, stats)
        }
        Err(e) => {
            tracing::error!("Failed to create agent: {:?}", e);
            return Err(DaemonError::SetupError(format!("Failed to create agent: {:?}", e)));
        }
    };

    tracing::info!("Agent created, starting tasks");

    // Spawn the agent runner
    let agent_handle = tokio::spawn(runner.run());

    // Spawn IPC server
    let ipc_handle = {
        let server = ipc_server.clone();
        tokio::spawn(async move {
            if let Err(e) = server.run().await {
                tracing::error!("IPC server error: {}", e);
            }
        })
    };

    // Spawn stats broadcaster
    let stats_handle = {
        let event_tx = event_tx.clone();
        let token = cancel_token.clone();
        tokio::spawn(broadcast_stats(stats, event_tx, token))
    };

    // Spawn agent data fetcher/broadcaster
    let data_handle = {
        let event_tx = event_tx.clone();
        let token = cancel_token.clone();
        tokio::spawn(broadcast_agent_data(api, lookup, event_tx, token, start_time))
    };

    // Wait for shutdown signal (Ctrl+C, stop command, or agent completion)
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received Ctrl+C, shutting down");
        }
        _ = cancel_token.cancelled() => {
            tracing::info!("Shutdown requested via IPC");
        }
        _ = agent_handle => {
            tracing::info!("Agent task completed");
        }
    }

    // Signal shutdown to all tasks
    cancel_token.cancel();

    // Wait for tasks to complete
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        let _ = ipc_handle.await;
        let _ = stats_handle.await;
        let _ = data_handle.await;
    })
    .await;

    tracing::info!("Daemon shutdown complete");
    Ok(())
}

/// Get secret code, waiting if necessary
async fn get_secret(secret: &mut PlayitSecret) -> Result<String, String> {
    // Try to get existing secret
    match secret.get().await {
        Ok(code) => return Ok(code),
        Err(e) => {
            tracing::warn!("No valid secret found: {:?}", e);
        }
    }

    // For daemon mode, we don't do interactive setup
    // The user should run the CLI to set up the secret first
    Err(format!(
        "No valid secret found. Please run '{}' to set up the agent first.",
        *crate::EXE_NAME
    ))
}

/// Broadcast stats at regular intervals
async fn broadcast_stats(
    stats: AgentStats,
    event_tx: broadcast::Sender<ServiceEvent>,
    cancel_token: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let snapshot = stats.snapshot();
                let event = ServiceEvent::Stats {
                    bytes_in: snapshot.bytes_in,
                    bytes_out: snapshot.bytes_out,
                    active_tcp: snapshot.active_tcp,
                    active_udp: snapshot.active_udp,
                };
                // Ignore send errors (no subscribers)
                let _ = event_tx.send(event);
            }
            _ = cancel_token.cancelled() => {
                break;
            }
        }
    }
}

/// Fetch and broadcast agent data at regular intervals
async fn broadcast_agent_data(
    api: playit_api_client::PlayitApi,
    lookup: Arc<OriginLookup>,
    event_tx: broadcast::Sender<ServiceEvent>,
    cancel_token: CancellationToken,
    start_time: u64,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(3));
    let mut guest_login_link: Option<(String, u64)> = None;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                match api.v1_agents_rundata().await {
                    Ok(mut api_data) => {
                        lookup.update_from_run_data(&api_data).await;

                        // Build agent data
                        let account_status = match api_data.permissions.account_status {
                            AccountStatus::Guest => AccountStatusInfo::Guest,
                            AccountStatus::EmailNotVerified => AccountStatusInfo::EmailNotVerified,
                            AccountStatus::Verified => AccountStatusInfo::Verified,
                        };

                        // Get login link for guest accounts
                        let login_link = match api_data.permissions.account_status {
                            AccountStatus::Guest => {
                                let now = now_milli();
                                match &guest_login_link {
                                    Some((link, ts)) if now - *ts < 15_000 => Some(link.clone()),
                                    _ => {
                                        if let Ok(session) = api.login_guest().await {
                                            let link = format!(
                                                "https://playit.gg/login/guest-account/{}",
                                                session.session_key
                                            );
                                            guest_login_link = Some((link.clone(), now_milli()));
                                            Some(link)
                                        } else {
                                            None
                                        }
                                    }
                                }
                            }
                            _ => None,
                        };

                        api_data.notices.sort_by_key(|n| n.priority);

                        let notices: Vec<NoticeInfo> = api_data
                            .notices
                            .iter()
                            .map(|n| NoticeInfo {
                                priority: format!("{:?}", n.priority),
                                message: n.message.to_string(),
                                resolve_link: n.resolve_link.as_ref().map(|s| s.to_string()),
                            })
                            .collect();

                        let tunnels: Vec<TunnelInfo> = api_data
                            .tunnels
                            .iter()
                            .filter_map(|tunnel| {
                                let origin = OriginResource::from_agent_tunnel(tunnel)?;

                                let destination = match origin.target {
                                    OriginTarget::Https {
                                        ip,
                                        http_port,
                                        https_port,
                                    } => format!("{ip} (http: {http_port}, https: {https_port})"),
                                    OriginTarget::Port { ip, port } => SocketAddr::new(ip, port).to_string(),
                                };

                                Some(TunnelInfo {
                                    display_address: tunnel.display_address.clone(),
                                    destination,
                                    is_disabled: tunnel.disabled_reason.is_some(),
                                    disabled_reason: tunnel.disabled_reason.as_ref().map(|s| s.to_string()),
                                })
                            })
                            .collect();

                        let pending_tunnels: Vec<PendingTunnelInfo> = api_data
                            .pending
                            .iter()
                            .map(|p| PendingTunnelInfo {
                                id: p.id.to_string(),
                                status_msg: p.status_msg.clone(),
                            })
                            .collect();

                        let agent_data = AgentData {
                            version: env!("CARGO_PKG_VERSION").to_string(),
                            tunnels,
                            pending_tunnels,
                            notices,
                            account_status,
                            agent_id: api_data.agent_id.to_string(),
                            login_link,
                            start_time,
                        };

                        let event = ServiceEvent::from(&agent_data);
                        let _ = event_tx.send(event);
                    }
                    Err(error) => {
                        tracing::error!(?error, "Failed to load agent data");
                    }
                }
            }
            _ = cancel_token.cancelled() => {
                break;
            }
        }
    }
}
