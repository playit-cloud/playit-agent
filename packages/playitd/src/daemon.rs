use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use playit_agent_core::agent_control::platform::current_platform;
use playit_agent_core::agent_control::version::{help_register_version, register_platform};
use playit_agent_core::network::origin_lookup::{OriginLookup, OriginResource, OriginTarget};
use playit_agent_core::network::tcp::tcp_settings::TcpSettings;
use playit_agent_core::network::udp::udp_settings::UdpSettings;
use playit_agent_core::playit_agent::{PlayitAgent, PlayitAgentSettings};
use playit_agent_core::stats::AgentStats;
use playit_agent_core::utils::now_milli;
use playit_api_client::PlayitApi;
use playit_api_client::api::{AccountStatus, Platform};
use playit_ipc::ipc::{IpcError, get_default_socket_path, protocol_info};
use playit_ipc::model::{
    AccountStatus as ServiceAccountStatus, AgentLifecycle, AgentState, ConnectionStats,
    NoticeState, PendingTunnelState, ServiceError, ServiceErrorCode, ServicePhase,
    ServiceStatus, ServiceUpdate, TunnelState,
};
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use crate::logging::IpcBroadcastLayer;
use crate::ipc_server::{IpcServer, SecretProvisionRequest, StateCache};

pub const DEFAULT_VARIANT_ID: &str = "308943e8-faef-4835-a2ba-270351f72aa3";

#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub secret: Option<String>,
    pub secret_path: Option<PathBuf>,
    pub socket_path: Option<String>,
    pub log_path: Option<PathBuf>,
    pub platform_docker: bool,
    pub version: VersionDetails,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            secret: None,
            secret_path: Some(default_secret_path()),
            socket_path: None,
            log_path: None,
            platform_docker: false,
            version: VersionDetails::from_cargo_package()
                .expect("Cargo package version must be a valid semver triplet"),
        }
    }
}

#[derive(Debug, Clone)]
enum SecretSource {
    Inline { secret: String },
    File { path: PathBuf },
}

#[derive(Debug, Clone)]
enum LoadedSecret {
    Ready(String),
    Missing,
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct VersionDetails {
    pub variant_id: String,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl VersionDetails {
    pub fn from_cargo_package() -> Result<Self, String> {
        Self::from_version_string(env!("CARGO_PKG_VERSION"), DEFAULT_VARIANT_ID)
    }

    pub fn from_version_string(version: &str, variant_id: &str) -> Result<Self, String> {
        let mut parts = version.split('-').next().unwrap_or(version).split('.');
        let major = parts
            .next()
            .ok_or_else(|| format!("missing major version in {version}"))
            .and_then(parse_version_part)?;
        let minor = parts
            .next()
            .ok_or_else(|| format!("missing minor version in {version}"))
            .and_then(parse_version_part)?;
        let patch = parts
            .next()
            .ok_or_else(|| format!("missing patch version in {version}"))
            .and_then(parse_version_part)?;

        Ok(Self {
            variant_id: variant_id.to_string(),
            major,
            minor,
            patch,
        })
    }

    pub fn apply_overrides(&mut self, overrides: VersionOverrideFile) {
        if let Some(variant_id) = overrides.variant_id {
            self.variant_id = variant_id;
        }
        if let Some(major) = overrides.major {
            self.major = major;
        }
        if let Some(minor) = overrides.minor {
            self.minor = minor;
        }
        if let Some(patch) = overrides.patch {
            self.patch = patch;
        }
    }

    pub fn version_string(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct VersionOverrideFile {
    pub variant_id: Option<String>,
    pub major: Option<u32>,
    pub minor: Option<u32>,
    pub patch: Option<u32>,
}

#[derive(Debug)]
pub enum DaemonError {
    Ipc(IpcError),
    SecretError(String),
    SetupError(String),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ipc(e) => write!(f, "IPC error: {e}"),
            Self::SecretError(e) => write!(f, "Secret error: {e}"),
            Self::SetupError(e) => write!(f, "Setup error: {e}"),
        }
    }
}

impl std::error::Error for DaemonError {}

impl From<IpcError> for DaemonError {
    fn from(e: IpcError) -> Self {
        Self::Ipc(e)
    }
}

pub async fn load_version_overrides(path: &Path) -> Result<VersionOverrideFile, String> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|error| format!("Failed to read version override file {}: {error}", path.display()))?;

    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("json") => serde_json::from_str(&content)
            .map_err(|error| format!("Invalid JSON in {}: {error}", path.display())),
        Some("yaml") | Some("yml") => serde_yaml::from_str(&content)
            .map_err(|error| format!("Invalid YAML in {}: {error}", path.display())),
        _ => Err(format!(
            "Unsupported version override file format for {}. Use .json, .yaml, or .yml",
            path.display()
        )),
    }
}

pub fn default_secret_path() -> PathBuf {
    if Path::new("playit.toml").exists() {
        return PathBuf::from("playit.toml");
    }

    #[cfg(target_os = "linux")]
    if Path::new("/etc/playit/playit.toml").exists() {
        return PathBuf::from("/etc/playit/playit.toml");
    }

    dirs::config_local_dir()
        .unwrap_or_else(|| ".".into())
        .join("playit_gg")
        .join("playit.toml")
}

#[cfg(target_os = "windows")]
pub fn windows_service_data_dir() -> PathBuf {
    std::env::var_os("PROGRAMDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("playit_gg")
}

#[cfg(target_os = "windows")]
pub fn windows_service_secret_path() -> PathBuf {
    windows_service_data_dir().join("playit.toml")
}

#[cfg(target_os = "windows")]
pub fn windows_service_log_path() -> PathBuf {
    windows_service_data_dir().join("logs").join("playitd.log")
}

impl SecretSource {
    fn from_options(options: &DaemonOptions) -> Self {
        match options.secret.clone() {
            Some(secret) => Self::Inline { secret },
            None => Self::File {
                path: options
                    .secret_path
                    .clone()
                    .unwrap_or_else(default_secret_path),
            },
        }
    }

    fn secret_path(&self) -> Option<&Path> {
        match self {
            Self::Inline { .. } => None,
            Self::File { path } => Some(path.as_path()),
        }
    }

    fn allows_ipc_provisioning(&self) -> bool {
        matches!(self, Self::File { .. })
    }

    async fn load(&self) -> LoadedSecret {
        match self {
            Self::Inline { secret } => match validate_secret(secret.trim()) {
                Ok(secret) => LoadedSecret::Ready(secret),
                Err(error) => LoadedSecret::Invalid(format!(
                    "Invalid secret passed via --secret: {error}"
                )),
            },
            Self::File { path } => load_secret_from_path(path).await,
        }
    }

    fn secret_provision_error(&self) -> ServiceError {
        match self {
            Self::Inline { .. } => daemon_error(
                ServiceErrorCode::SecretPinned,
                "Secret provisioning is disabled because playitd was started with --secret"
                    .to_string(),
                false,
            ),
            Self::File { .. } => daemon_error(
                ServiceErrorCode::ProvisioningUnavailable,
                "Secret provisioning is unavailable".to_string(),
                true,
            ),
        }
    }

    fn secret_reset_error(&self) -> ServiceError {
        match self {
            Self::Inline { .. } => daemon_error(
                ServiceErrorCode::SecretPinned,
                "Secret reset is disabled because playitd was started with --secret".to_string(),
                false,
            ),
            Self::File { path } => daemon_error(
                ServiceErrorCode::SecretWriteFailed,
                format!("Failed to access secret file {}", path.display()),
                true,
            ),
        }
    }
}

pub async fn run_daemon(options: DaemonOptions) -> Result<(), DaemonError> {
    let start_time = now_milli();
    let (event_tx, _) = broadcast::channel::<ServiceUpdate>(256);
    let version_string = options.version.version_string();
    let platform = if options.platform_docker {
        Platform::Docker
    } else {
        current_platform()
    };
    let secret_source = SecretSource::from_options(&options);
    let status_context = StatusContext {
        secret_path: secret_source
            .secret_path()
            .map(|path| path.display().to_string()),
        socket_path: options
            .socket_path
            .clone()
            .unwrap_or_else(|| get_default_socket_path().to_string()),
        version: version_string.clone(),
        start_time,
    };

    let log_filter =
        EnvFilter::try_from_env("PLAYIT_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let use_ansi = matches!(platform, Platform::Linux | Platform::Docker);
    let _log_guard = init_tracing(log_filter, use_ansi, event_tx.clone(), options.log_path.as_deref())
        .map_err(DaemonError::SetupError)?;

    register_platform(platform);

    let _ = help_register_version(&version_string, &options.version.variant_id);

    tracing::info!(
        socket_path = ?options.socket_path,
        secret_path = ?status_context.secret_path,
        version = %version_string,
        "Starting playitd"
    );

    let cancel_token = CancellationToken::new();
    let (secret_provision_tx, mut secret_rx) = if secret_source.allows_ipc_provisioning() {
        let (secret_tx, secret_rx) = mpsc::channel::<SecretProvisionRequest>(8);
        (Some(secret_tx), Some(secret_rx))
    } else {
        (None, None)
    };
    let ipc_server = Arc::new(
        IpcServer::new_with_sender(
            options.socket_path.clone(),
            cancel_token.clone(),
            event_tx.clone(),
            secret_source.secret_path().map(PathBuf::from),
            secret_provision_tx,
            secret_source.secret_provision_error(),
            secret_source.secret_reset_error(),
        )
            .await
            .map_err(DaemonError::Ipc)?,
    );
    let listener = ipc_server.bind_listener().await.map_err(DaemonError::Ipc)?;
    let state_cache = ipc_server.state_cache();
    let event_tx = ipc_server.event_sender();
    let ipc_handle = {
        let server = ipc_server.clone();
        tokio::spawn(async move {
            if let Err(e) = server.run(listener).await {
                tracing::error!("IPC server error: {e}");
            }
        })
    };

    let secret_code = match secret_source.load().await {
        LoadedSecret::Ready(secret) => {
            publish_runtime_state(
                &state_cache,
                &event_tx,
                status_context.status(ServicePhase::Starting, true, None),
                AgentLifecycle::Starting,
            )
            .await;
            secret
        }
        LoadedSecret::Missing => {
            let Some(secret_path) = secret_source.secret_path() else {
                return Err(DaemonError::SecretError(
                    "No secret source available for startup".to_string(),
                ));
            };
            publish_runtime_state(
                &state_cache,
                &event_tx,
                status_context.status(ServicePhase::WaitingForSecret, false, None),
                AgentLifecycle::WaitingForSecret,
            )
            .await;

            match wait_for_secret_provisioning(
                secret_path,
                secret_rx
                    .as_mut()
                    .expect("file-backed secret mode must enable provisioning"),
                &cancel_token,
            )
                .await
                .map_err(DaemonError::SecretError)?
            {
                Some(secret) => {
                    publish_runtime_state(
                        &state_cache,
                        &event_tx,
                        status_context.status(ServicePhase::Starting, true, None),
                        AgentLifecycle::Starting,
                    )
                    .await;
                    secret
                }
                None => {
                    publish_runtime_state(
                        &state_cache,
                        &event_tx,
                        status_context.status(ServicePhase::Stopping, false, None),
                        AgentLifecycle::Stopping,
                    )
                    .await;
                    let _ = ipc_handle.await;
                    tracing::info!("playitd shutdown before provisioning completed");
                    return Ok(());
                }
            }
        }
        LoadedSecret::Invalid(error) => {
            let service_error = daemon_error(
                ServiceErrorCode::InvalidSecret,
                error.clone(),
                secret_source.allows_ipc_provisioning(),
            );
            publish_runtime_state(
                &state_cache,
                &event_tx,
                status_context.status(
                    ServicePhase::HasInvalidSecret,
                    false,
                    Some(service_error.clone()),
                ),
                AgentLifecycle::HasInvalidSecret(service_error.clone()),
            )
            .await;

            if !secret_source.allows_ipc_provisioning() {
                return Err(DaemonError::SecretError(error));
            }

            let secret_path = secret_source
                .secret_path()
                .expect("file-backed secret mode must provide a secret path");
            match wait_for_secret_provisioning(
                secret_path,
                secret_rx
                    .as_mut()
                    .expect("file-backed secret mode must enable provisioning"),
                &cancel_token,
            )
            .await
            .map_err(DaemonError::SecretError)?
            {
                Some(secret) => {
                    publish_runtime_state(
                        &state_cache,
                        &event_tx,
                        status_context.status(ServicePhase::Starting, true, None),
                        AgentLifecycle::Starting,
                    )
                    .await;
                    secret
                }
                None => {
                    publish_runtime_state(
                        &state_cache,
                        &event_tx,
                        status_context.status(ServicePhase::Stopping, false, None),
                        AgentLifecycle::Stopping,
                    )
                    .await;
                    let _ = ipc_handle.await;
                    tracing::info!("playitd shutdown before invalid secret was corrected");
                    return Ok(());
                }
            }
        }
    };

    let api = PlayitApi::create(api_base(), Some(secret_code.clone()));
    ipc_server.set_api(api.clone()).await;

    let lookup = Arc::new(OriginLookup::default());
    if let Ok(data) = api.v1_agents_rundata().await {
        lookup.update_from_run_data(&data).await;
    }

    let settings = PlayitAgentSettings {
        udp_settings: UdpSettings::default(),
        tcp_settings: TcpSettings::default(),
        api_url: api_base(),
        secret_key: secret_code,
    };

    let (runner, stats) = match PlayitAgent::new(settings, lookup.clone()).await {
        Ok(runner) => {
            let stats = runner.stats();
            (runner, stats)
        }
        Err(error) => {
            let message = format!("Failed to create agent: {error:?}");
            let service_error = daemon_error(ServiceErrorCode::Internal, message.clone(), true);
            publish_runtime_state(
                &state_cache,
                &event_tx,
                status_context.status(
                    ServicePhase::Error,
                    true,
                    Some(service_error.clone()),
                ),
                AgentLifecycle::Error(service_error),
            )
            .await;
            return Err(DaemonError::SetupError(message));
        }
    };

    publish_runtime_state(
        &state_cache,
        &event_tx,
        status_context.status(ServicePhase::Running, true, None),
        AgentLifecycle::Starting,
    )
    .await;

    let agent_handle = tokio::spawn(runner.run());
    let stats_handle = {
        let event_tx = event_tx.clone();
        let token = cancel_token.clone();
        let cache = state_cache.clone();
        tokio::spawn(broadcast_stats(stats, event_tx, cache, token))
    };
    let state_handle = {
        let event_tx = event_tx.clone();
        let token = cancel_token.clone();
        let cache = state_cache.clone();
        tokio::spawn(broadcast_agent_state(
            api,
            lookup,
            event_tx,
            cache,
            token,
            start_time,
            version_string,
        ))
    };

    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("Received Ctrl+C, shutting down"),
        _ = cancel_token.cancelled() => tracing::info!("Shutdown requested via IPC"),
        _ = agent_handle => tracing::info!("Agent task completed"),
    }

    cancel_token.cancel();
    publish_runtime_state(
        &state_cache,
        &event_tx,
        status_context.status(ServicePhase::Stopping, true, None),
        AgentLifecycle::Stopping,
    )
    .await;

    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        let _ = ipc_handle.await;
        let _ = stats_handle.await;
        let _ = state_handle.await;
    })
    .await;

    tracing::info!("playitd shutdown complete");
    Ok(())
}

async fn broadcast_stats(
    stats: AgentStats,
    event_tx: broadcast::Sender<ServiceUpdate>,
    state_cache: Arc<StateCache>,
    cancel_token: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let snapshot = stats.snapshot();
                let stats = ConnectionStats {
                    bytes_in: snapshot.bytes_in,
                    bytes_out: snapshot.bytes_out,
                    active_tcp: snapshot.active_tcp,
                    active_udp: snapshot.active_udp,
                };
                state_cache.set_stats(stats.clone()).await;
                let _ = event_tx.send(ServiceUpdate::Stats(stats));
            }
            _ = cancel_token.cancelled() => break,
        }
    }
}

async fn broadcast_agent_state(
    api: PlayitApi,
    lookup: Arc<OriginLookup>,
    event_tx: broadcast::Sender<ServiceUpdate>,
    state_cache: Arc<StateCache>,
    cancel_token: CancellationToken,
    start_time: u64,
    version_string: String,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(3));
    let mut guest_login_link: Option<(String, u64)> = None;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                match api.v1_agents_rundata().await {
                    Ok(mut api_data) => {
                        lookup.update_from_run_data(&api_data).await;

                        let login_link = match api_data.permissions.account_status {
                            AccountStatus::Guest => {
                                get_cached_guest_login_link(&api, &mut guest_login_link).await
                            }
                            _ => None,
                        };

                        api_data.notices.sort_by_key(|n| n.priority);

                        let state = AgentState {
                            version: version_string.clone(),
                            tunnels: api_data
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

                                    Some(TunnelState {
                                        display_address: tunnel.display_address.clone(),
                                        destination,
                                        is_disabled: tunnel.disabled_reason.is_some(),
                                        disabled_reason: tunnel.disabled_reason.as_ref().map(|s| s.to_string()),
                                    })
                                })
                                .collect(),
                            pending_tunnels: api_data
                                .pending
                                .iter()
                                .map(|p| PendingTunnelState {
                                    id: p.id.to_string(),
                                    status_msg: p.status_msg.clone(),
                                })
                                .collect(),
                            notices: api_data
                                .notices
                                .iter()
                                .map(|n| NoticeState {
                                    priority: format!("{:?}", n.priority),
                                    message: n.message.to_string(),
                                    resolve_link: n.resolve_link.as_ref().map(|s| s.to_string()),
                                })
                                .collect(),
                            account_status: match api_data.permissions.account_status {
                                AccountStatus::Guest => ServiceAccountStatus::Guest,
                                AccountStatus::EmailNotVerified => ServiceAccountStatus::EmailNotVerified,
                                AccountStatus::Verified => ServiceAccountStatus::Verified,
                            },
                            agent_id: api_data.agent_id.to_string(),
                            login_link,
                            start_time,
                        };

                        let lifecycle = AgentLifecycle::Running(state);
                        state_cache.set_lifecycle(lifecycle.clone()).await;
                        let _ = event_tx.send(ServiceUpdate::Lifecycle(lifecycle));
                    }
                    Err(error) => tracing::error!(?error, "Failed to load agent data"),
                }
            }
            _ = cancel_token.cancelled() => break,
        }
    }
}

fn api_base() -> String {
    dotenv::var("API_BASE").unwrap_or_else(|_| "https://api.playit.gg".to_string())
}

async fn get_cached_guest_login_link(
    api: &PlayitApi,
    guest_login_link: &mut Option<(String, u64)>,
) -> Option<String> {
    let now = now_milli();
    match guest_login_link {
        Some((link, ts)) if now.saturating_sub(*ts) < 15_000 => Some(link.clone()),
        _ => match api.login_guest().await {
            Ok(session) => {
                let link = format!(
                    "https://playit.gg/login/guest-account/{}",
                    session.session_key
                );
                *guest_login_link = Some((link.clone(), now));
                Some(link)
            }
            Err(_) => None,
        },
    }
}

async fn load_secret_from_path(path: &Path) -> LoadedSecret {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return LoadedSecret::Missing,
        Err(error) => {
            return LoadedSecret::Invalid(format!(
                "Failed to read secret file {}: {error}",
                path.display()
            ))
        }
    };

    match parse_secret_file(&content) {
        Ok(secret) => LoadedSecret::Ready(secret),
        Err(()) => LoadedSecret::Invalid(format!(
            "Invalid secret file at {}. Remove or replace it with a valid secret.",
            path.display()
        )),
    }
}

async fn wait_for_secret_provisioning(
    secret_path: &Path,
    provision_rx: &mut mpsc::Receiver<SecretProvisionRequest>,
    cancel_token: &CancellationToken,
) -> Result<Option<String>, String> {
    tracing::info!(
        secret_path = %secret_path.display(),
        "Waiting for frontend secret provisioning over IPC"
    );

    loop {
        tokio::select! {
            maybe_request = provision_rx.recv() => {
                let Some(request) = maybe_request else {
                    return Err("Secret provisioning channel closed".to_string());
                };

                let result = persist_secret_file(secret_path, &request.secret).await;
                let ack = result.as_ref().map(|_| ()).map_err(Clone::clone);
                let _ = request.response_tx.send(ack);

                match result {
                    Ok(()) => {
                        tracing::info!(secret_path = %secret_path.display(), "Secret provisioned successfully");
                        return Ok(Some(request.secret));
                    }
                    Err(error) => {
                        tracing::error!(secret_path = %secret_path.display(), "{error}");
                    }
                }
            }
            _ = cancel_token.cancelled() => {
                return Ok(None);
            }
        }
    }
}

async fn persist_secret_file(path: &Path, secret: &str) -> Result<(), String> {
    let secret = validate_secret(secret.trim())?;

    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("Failed to create secret directory {}: {error}", parent.display()))?;
    }

    let content = if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        toml::to_string(&SecretConfig {
            secret_key: secret.clone(),
        })
        .map_err(|error| format!("Failed to serialize secret file {}: {error}", path.display()))?
    } else {
        secret
    };

    tokio::fs::write(path, content)
        .await
        .map_err(|error| format!("Failed to write secret file {}: {error}", path.display()))
}

fn parse_secret_file(content: &str) -> Result<String, ()> {
    let trimmed = content.trim();
    if let Ok(secret) = validate_secret(trimmed) {
        return Ok(secret);
    }

    let config = toml::from_str::<SecretConfig>(content).map_err(|_| ())?;
    validate_secret(config.secret_key.trim()).map_err(|_| ())
}

fn validate_secret(secret: &str) -> Result<String, String> {
    hex::decode(secret)
        .map(|_| secret.to_string())
        .map_err(|_| "Malformed secret".to_string())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SecretConfig {
    secret_key: String,
}

fn parse_version_part(part: &str) -> Result<u32, String> {
    u32::from_str(part).map_err(|error| format!("Invalid version component {part}: {error}"))
}

struct StatusContext {
    secret_path: Option<String>,
    socket_path: String,
    version: String,
    start_time: u64,
}

impl StatusContext {
    fn status(
        &self,
        phase: ServicePhase,
        has_secret: bool,
        last_error: Option<ServiceError>,
    ) -> ServiceStatus {
        ServiceStatus {
            phase,
            pid: std::process::id(),
            uptime_secs: now_milli().saturating_sub(self.start_time) / 1000,
            version: self.version.clone(),
            socket_path: self.socket_path.clone(),
            secret_path: self.secret_path.clone(),
            has_secret,
            protocol: protocol_info(),
            last_error,
        }
    }
}

async fn publish_runtime_state(
    state_cache: &Arc<StateCache>,
    event_tx: &broadcast::Sender<ServiceUpdate>,
    status: ServiceStatus,
    lifecycle: AgentLifecycle,
) {
    state_cache.set_status(status.clone()).await;
    state_cache.set_lifecycle(lifecycle.clone()).await;
    let _ = event_tx.send(ServiceUpdate::Status(status));
    let _ = event_tx.send(ServiceUpdate::Lifecycle(lifecycle));
}

fn daemon_error(code: ServiceErrorCode, message: String, retryable: bool) -> ServiceError {
    ServiceError {
        code,
        message,
        retryable,
        details: None,
    }
}

fn init_tracing(
    log_filter: EnvFilter,
    use_ansi: bool,
    event_tx: broadcast::Sender<ServiceUpdate>,
    log_path: Option<&Path>,
) -> Result<Option<WorkerGuard>, String> {
    match log_path {
        Some(path) => {
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Failed to create log directory {}: {error}", parent.display()))?;
            let file_name = path
                .file_name()
                .and_then(|file| file.to_str())
                .ok_or_else(|| format!("Invalid --log-path {}", path.display()))?;
            let writer = tracing_appender::rolling::never(parent, file_name);
            let (non_blocking, guard) = tracing_appender::non_blocking(writer);

            tracing_subscriber::registry()
                .with(log_filter)
                .with(IpcBroadcastLayer::new(event_tx))
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(use_ansi)
                        .with_writer(non_blocking),
                )
                .init();

            Ok(Some(guard))
        }
        None => {
            tracing_subscriber::registry()
                .with(log_filter)
                .with(IpcBroadcastLayer::new(event_tx))
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(use_ansi)
                        .with_writer(std::io::stderr),
                )
                .init();

            Ok(None)
        }
    }
}
