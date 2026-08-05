use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::errors::{DaemonError, SecretError};
use crate::guest_login::GuestLoginCache;
use crate::ipc_server::{IpcServer, SecretProvisionRequest, StateCache};
use crate::logging::init_tracing;
use crate::publisher::{
    AgentStatePublisher, StatusContext, broadcast_stats, publish_runtime_state,
};
use playit_agent_core::agent_control::errors::SetupError;
use playit_agent_core::agent_control::platform::current_platform;
use playit_agent_core::agent_control::version::parse_agent_version;
use playit_agent_core::network::origin_lookup::OriginLookup;
use playit_agent_core::network::tcp::tcp_settings::TcpSettings;
use playit_agent_core::network::udp::udp_settings::UdpSettings;
use playit_agent_core::playit_agent::{PlayitAgent, PlayitAgentSettings};
use playit_agent_core::stats::AgentStats;
use playit_agent_core::utils::now_milli;
use playit_api_client::api::{
    AgentVersion, ApiResponseError, AuthError, Platform, ProtoRegisterError,
};
use playit_api_client::{PlayitApi, default_api_base};
use playit_ipc::agent_over_limit_guidance;
use playit_ipc::ipc::get_default_socket_path;
use playit_ipc::model::{
    AgentLifecycle, ServiceError, ServiceErrorCode, ServicePhase, ServiceUpdate,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::version::VersionDetails;
const AGENT_LIMIT_RETRY_INTERVAL: Duration = Duration::from_secs(30);
#[cfg(target_os = "windows")]
const WINDOWS_LOG_MAX_FILE_SIZE_BYTES: u64 = 5 * 1024 * 1024;
#[cfg(target_os = "windows")]
const WINDOWS_LOG_MAX_TOTAL_FILES: usize = 3;
#[cfg(target_os = "windows")]
const WINDOWS_LOG_MAX_ROTATED_FILES: usize = WINDOWS_LOG_MAX_TOTAL_FILES - 1;

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
            secret_path: Some(crate::paths::default_secret_path()),
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

impl SecretSource {
    fn from_options(options: &DaemonOptions) -> Self {
        match options.secret.clone() {
            Some(secret) => Self::Inline { secret },
            None => Self::File {
                path: options
                    .secret_path
                    .clone()
                    .unwrap_or_else(crate::paths::default_secret_path),
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
                Err(error) => {
                    LoadedSecret::Invalid(format!("Invalid secret passed via --secret: {error}"))
                }
            },
            Self::File { path } => load_secret_from_path(path).await,
        }
    }

    fn secret_provision_error(&self) -> ServiceError {
        match self {
            Self::Inline { .. } => daemon_error(
                ServiceErrorCode::SecretPinned,
                "Secret provisioning is unavailable because playitd was started with --secret."
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
                "Secret reset is unavailable because playitd was started with --secret."
                    .to_string(),
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
    let secret_source = SecretSource::from_options(&options);
    let (secret_provision_tx, mut secret_rx) = if secret_source.allows_ipc_provisioning() {
        let (secret_tx, secret_rx) = mpsc::channel::<SecretProvisionRequest>(8);
        (Some(secret_tx), Some(secret_rx))
    } else {
        (None, None)
    };
    let mut runtime = initialize_runtime(&options, &secret_source, secret_provision_tx).await?;

    let Some(secret_code) =
        resolve_startup_secret(&mut runtime, &secret_source, &mut secret_rx).await?
    else {
        return Ok(());
    };

    let Some(agent) =
        build_agent_with_reprovisioning(&mut runtime, &secret_source, &mut secret_rx, secret_code)
            .await?
    else {
        return Ok(());
    };

    publish_runtime_state(
        &runtime.state_cache,
        &runtime.event_tx,
        runtime
            .status_context
            .status(ServicePhase::Running, true, None),
        AgentLifecycle::Starting,
    )
    .await;

    run_until_shutdown(runtime, agent).await
}

struct DaemonRuntime {
    start_time: u64,
    version_string: String,
    status_context: StatusContext,
    cancel_token: CancellationToken,
    ipc_server: Arc<IpcServer>,
    ipc_handle: Option<JoinHandle<()>>,
    state_cache: Arc<StateCache>,
    event_tx: broadcast::Sender<ServiceUpdate>,
    _log_guard: Option<WorkerGuard>,
    agent_version: AgentVersion,
    platform: Platform,
    guest_login_cache: Arc<GuestLoginCache>,
}

struct AgentRuntime {
    api: PlayitApi,
    runner: PlayitAgent,
    stats: AgentStats,
    lookup: Arc<OriginLookup>,
}

async fn initialize_runtime(
    options: &DaemonOptions,
    secret_source: &SecretSource,
    secret_provision_tx: Option<mpsc::Sender<SecretProvisionRequest>>,
) -> Result<DaemonRuntime, DaemonError> {
    let start_time = now_milli();
    let (event_tx, _) = broadcast::channel::<ServiceUpdate>(256);
    let version_string = options.version.version_string();
    let platform = if options.platform_docker {
        Platform::Docker
    } else {
        current_platform()
    };
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
    let log_guard = init_tracing(
        log_filter,
        use_ansi,
        event_tx.clone(),
        options.log_path.as_deref(),
    )
    .map_err(DaemonError::Logging)?;

    let agent_version = parse_agent_version(&version_string, &options.version.variant_id)
        .map_err(|error| DaemonError::Runtime(error.to_string()))?;
    let guest_login_cache = Arc::new(GuestLoginCache::default());

    tracing::info!(
        socket_path = ?options.socket_path,
        secret_path = ?status_context.secret_path,
        version = %version_string,
        "Starting playitd"
    );

    let cancel_token = CancellationToken::new();
    let ipc_server = Arc::new(
        IpcServer::new_with_sender(
            options.socket_path.clone(),
            cancel_token.clone(),
            event_tx,
            secret_source.secret_path().map(PathBuf::from),
            secret_provision_tx,
            secret_source.secret_provision_error(),
            secret_source.secret_reset_error(),
            guest_login_cache.clone(),
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

    Ok(DaemonRuntime {
        start_time,
        version_string,
        status_context,
        cancel_token,
        ipc_server,
        ipc_handle: Some(ipc_handle),
        state_cache,
        event_tx,
        _log_guard: log_guard,
        agent_version,
        platform,
        guest_login_cache,
    })
}

async fn resolve_startup_secret(
    runtime: &mut DaemonRuntime,
    secret_source: &SecretSource,
    secret_rx: &mut Option<mpsc::Receiver<SecretProvisionRequest>>,
) -> Result<Option<String>, DaemonError> {
    match secret_source.load().await {
        LoadedSecret::Ready(secret) => {
            publish_starting(runtime).await;
            Ok(Some(secret))
        }
        LoadedSecret::Missing => {
            let secret = wait_for_startup_secret(runtime, secret_source, secret_rx).await?;
            if secret.is_none() {
                tracing::info!("playitd shutdown before provisioning completed");
            }
            Ok(secret)
        }
        LoadedSecret::Invalid(error) => {
            let service_error = daemon_error(
                ServiceErrorCode::InvalidSecret,
                error.clone(),
                secret_source.allows_ipc_provisioning(),
            );
            publish_runtime_state(
                &runtime.state_cache,
                &runtime.event_tx,
                runtime.status_context.status(
                    ServicePhase::HasInvalidSecret,
                    false,
                    Some(service_error.clone()),
                ),
                AgentLifecycle::HasInvalidSecret(service_error),
            )
            .await;

            if !secret_source.allows_ipc_provisioning() {
                return Err(DaemonError::Secret(error.into()));
            }

            let secret = wait_for_startup_secret(runtime, secret_source, secret_rx).await?;
            if secret.is_none() {
                tracing::info!("playitd shutdown before invalid secret was corrected");
            }
            Ok(secret)
        }
    }
}

async fn wait_for_startup_secret(
    runtime: &mut DaemonRuntime,
    secret_source: &SecretSource,
    secret_rx: &mut Option<mpsc::Receiver<SecretProvisionRequest>>,
) -> Result<Option<String>, DaemonError> {
    let Some(secret_path) = secret_source.secret_path() else {
        return Err(DaemonError::Secret(
            "No secret source available for startup".into(),
        ));
    };

    publish_runtime_state(
        &runtime.state_cache,
        &runtime.event_tx,
        runtime
            .status_context
            .status(ServicePhase::WaitingForSecret, false, None),
        AgentLifecycle::WaitingForSecret,
    )
    .await;

    wait_for_provisioned_secret(runtime, secret_path, secret_rx).await
}

async fn wait_for_provisioned_secret(
    runtime: &mut DaemonRuntime,
    secret_path: &Path,
    secret_rx: &mut Option<mpsc::Receiver<SecretProvisionRequest>>,
) -> Result<Option<String>, DaemonError> {
    let secret_rx = secret_rx
        .as_mut()
        .expect("file-backed secret mode must enable provisioning");

    match wait_for_secret_provisioning(secret_path, secret_rx, &runtime.cancel_token)
        .await
        .map_err(DaemonError::Secret)?
    {
        Some(secret) => {
            publish_starting(runtime).await;
            Ok(Some(secret))
        }
        None => {
            publish_stopping(runtime, false).await;
            await_ipc_shutdown(runtime).await;
            Ok(None)
        }
    }
}

async fn build_agent_with_reprovisioning(
    runtime: &mut DaemonRuntime,
    secret_source: &SecretSource,
    secret_rx: &mut Option<mpsc::Receiver<SecretProvisionRequest>>,
    secret_code: String,
) -> Result<Option<AgentRuntime>, DaemonError> {
    let lookup = Arc::new(OriginLookup::default());
    let mut secret_code = secret_code;

    loop {
        let api = PlayitApi::create(default_api_base(), Some(secret_code.clone()));
        runtime.ipc_server.set_api(api.clone()).await;

        if let Ok(data) = api.v1_agents_rundata().await {
            lookup.update_from_run_data(&data).await;
        }

        let settings = PlayitAgentSettings {
            udp_settings: UdpSettings::default(),
            tcp_settings: TcpSettings::default(),
            api_url: default_api_base(),
            secret_key: secret_code.clone(),
            agent_version: runtime.agent_version.clone(),
            platform: runtime.platform,
        };

        match PlayitAgent::new(settings, lookup.clone()).await {
            Ok(runner) => {
                let stats = runner.stats();
                return Ok(Some(AgentRuntime {
                    api,
                    runner,
                    stats,
                    lookup,
                }));
            }
            Err(error)
                if secret_source.allows_ipc_provisioning()
                    && is_invalid_agent_secret_error(&error) =>
            {
                tracing::warn!(?error, "configured agent secret is no longer valid");

                let service_error = daemon_error(
                    ServiceErrorCode::InvalidSecret,
                    "The configured playit secret is no longer valid. Run `playit setup` to provision a new secret.".to_string(),
                    true,
                );
                publish_runtime_state(
                    &runtime.state_cache,
                    &runtime.event_tx,
                    runtime.status_context.status(
                        ServicePhase::WaitingForSecret,
                        false,
                        Some(service_error),
                    ),
                    AgentLifecycle::WaitingForSecret,
                )
                .await;

                let secret_path = secret_source
                    .secret_path()
                    .expect("file-backed secret mode must provide a secret path");
                let Some(secret) =
                    wait_for_provisioned_secret(runtime, secret_path, secret_rx).await?
                else {
                    tracing::info!("playitd shutdown before reprovisioning completed");
                    return Ok(None);
                };
                secret_code = secret;
            }
            Err(error) if is_agent_disabled_over_limit_error(&error) => {
                tracing::warn!(
                    ?error,
                    "agent disabled because the account is over the agent limit"
                );

                let service_error = daemon_error(
                    ServiceErrorCode::AgentDisabledOverLimit,
                    agent_disabled_over_limit_message(),
                    true,
                );
                publish_runtime_state(
                    &runtime.state_cache,
                    &runtime.event_tx,
                    runtime.status_context.status(
                        ServicePhase::DisabledOverLimit,
                        true,
                        Some(service_error.clone()),
                    ),
                    AgentLifecycle::DisabledOverLimit(service_error),
                )
                .await;

                tokio::select! {
                    _ = runtime.cancel_token.cancelled() => {
                        publish_stopping(runtime, true).await;
                        await_ipc_shutdown(runtime).await;
                        tracing::info!("playitd shutdown while waiting for the account agent limit to be resolved");
                        return Ok(None);
                    }
                    _ = tokio::time::sleep(AGENT_LIMIT_RETRY_INTERVAL) => {}
                }
            }
            Err(error) => {
                let message = setup_error_user_message(&error);
                tracing::error!(?error, %message, "failed to start playit agent");
                let service_error = daemon_error(ServiceErrorCode::Internal, message.clone(), true);
                publish_runtime_state(
                    &runtime.state_cache,
                    &runtime.event_tx,
                    runtime.status_context.status(
                        ServicePhase::Error,
                        true,
                        Some(service_error.clone()),
                    ),
                    AgentLifecycle::Error(service_error),
                )
                .await;
                return Err(DaemonError::Runtime(message));
            }
        }
    }
}

async fn run_until_shutdown(
    mut runtime: DaemonRuntime,
    agent: AgentRuntime,
) -> Result<(), DaemonError> {
    let agent_cancel = agent.runner.cancellation_token();
    let mut agent_handle = tokio::spawn(agent.runner.run());
    let stats_handle = {
        let event_tx = runtime.event_tx.clone();
        let token = runtime.cancel_token.clone();
        let cache = runtime.state_cache.clone();
        tokio::spawn(broadcast_stats(agent.stats, event_tx, cache, token))
    };
    let state_handle = {
        let event_tx = runtime.event_tx.clone();
        let token = runtime.cancel_token.clone();
        let cache = runtime.state_cache.clone();
        tokio::spawn(
            AgentStatePublisher {
                api: agent.api,
                lookup: agent.lookup,
                event_tx,
                state_cache: cache,
                cancel_token: token,
                start_time: runtime.start_time,
                version_string: runtime.version_string.clone(),
                guest_login_cache: runtime.guest_login_cache.clone(),
            }
            .run(),
        )
    };

    let shutdown_reason = tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received Ctrl+C, shutting down");
            ShutdownReason::Requested
        }
        _ = runtime.cancel_token.cancelled() => {
            tracing::info!("Shutdown requested via IPC");
            ShutdownReason::Requested
        }
        result = &mut agent_handle => {
            if runtime.cancel_token.is_cancelled() || agent_cancel.is_cancelled() {
                tracing::info!("Agent task completed during shutdown");
                ShutdownReason::Requested
            } else {
                match &result {
                    Ok(()) => tracing::error!("Agent task stopped unexpectedly"),
                    Err(error) => tracing::error!(?error, "Agent task failed"),
                }
                ShutdownReason::AgentStopped(result)
            }
        }
    };

    runtime.cancel_token.cancel();
    agent_cancel.cancel();
    publish_stopping(&runtime, true).await;

    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        if !matches!(shutdown_reason, ShutdownReason::AgentStopped(_)) {
            let _ = (&mut agent_handle).await;
        }
        await_ipc_shutdown(&mut runtime).await;
        let _ = stats_handle.await;
        let _ = state_handle.await;
    })
    .await;

    tracing::info!("playitd shutdown complete");

    match shutdown_reason {
        ShutdownReason::Requested => Ok(()),
        ShutdownReason::AgentStopped(Ok(())) => Err(DaemonError::Runtime(
            "playit agent task stopped unexpectedly".to_string(),
        )),
        ShutdownReason::AgentStopped(Err(error)) => Err(DaemonError::Runtime(format!(
            "playit agent task failed: {error}"
        ))),
    }
}

enum ShutdownReason {
    Requested,
    AgentStopped(Result<(), tokio::task::JoinError>),
}

async fn publish_starting(runtime: &DaemonRuntime) {
    publish_runtime_state(
        &runtime.state_cache,
        &runtime.event_tx,
        runtime
            .status_context
            .status(ServicePhase::Starting, true, None),
        AgentLifecycle::Starting,
    )
    .await;
}

async fn publish_stopping(runtime: &DaemonRuntime, has_secret: bool) {
    publish_runtime_state(
        &runtime.state_cache,
        &runtime.event_tx,
        runtime
            .status_context
            .status(ServicePhase::Stopping, has_secret, None),
        AgentLifecycle::Stopping,
    )
    .await;
}

async fn await_ipc_shutdown(runtime: &mut DaemonRuntime) {
    if let Some(handle) = runtime.ipc_handle.take() {
        let _ = handle.await;
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
            ));
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
) -> Result<Option<String>, SecretError> {
    tracing::info!(
        secret_path = %secret_path.display(),
        "Waiting for frontend secret provisioning over IPC"
    );

    loop {
        tokio::select! {
            maybe_request = provision_rx.recv() => {
                let Some(request) = maybe_request else {
                    return Err("Secret provisioning channel closed".into());
                };

                let result = persist_secret_file(secret_path, &request.secret).await;
                let ack = result.as_ref().map(|_| ()).map_err(ToString::to_string);
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

async fn persist_secret_file(path: &Path, secret: &str) -> Result<(), SecretError> {
    let secret = validate_secret(secret.trim())?;

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            SecretError(format!(
                "Failed to create secret directory {}: {error}",
                parent.display()
            ))
        })?;
    }

    let content = if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        toml::to_string(&SecretConfig {
            secret_key: secret.clone(),
        })
        .map_err(|error| {
            SecretError(format!(
                "Failed to serialize secret file {}: {error}",
                path.display()
            ))
        })?
    } else {
        secret
    };

    secure_write_secret_file(path, content.as_bytes()).await
}

#[cfg(unix)]
async fn secure_write_secret_file(path: &Path, content: &[u8]) -> Result<(), SecretError> {
    let path = path.to_path_buf();
    let content = content.to_vec();

    tokio::task::spawn_blocking(move || secure_write_secret_file_blocking(&path, &content))
        .await
        .map_err(|error| SecretError(format!("Failed to join secret file writer task: {error}")))?
}

#[cfg(unix)]
fn secure_write_secret_file_blocking(path: &Path, content: &[u8]) -> Result<(), SecretError> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("playit.toml");
    let tmp_path = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        now_milli()
    ));

    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp_path)
            .map_err(|error| {
                SecretError(format!(
                    "Failed to create temporary secret file {}: {error}",
                    tmp_path.display()
                ))
            })?;

        file.write_all(content).map_err(|error| {
            SecretError(format!(
                "Failed to write temporary secret file {}: {error}",
                tmp_path.display()
            ))
        })?;
        file.sync_all().map_err(|error| {
            SecretError(format!(
                "Failed to sync temporary secret file {}: {error}",
                tmp_path.display()
            ))
        })?;
        drop(file);

        std::fs::rename(&tmp_path, path).map_err(|error| {
            SecretError(format!(
                "Failed to replace secret file {} with {}: {error}",
                path.display(),
                tmp_path.display()
            ))
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| {
                SecretError(format!(
                    "Failed to set secret file permissions on {}: {error}",
                    path.display()
                ))
            },
        )?;

        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }

    result
}

#[cfg(not(unix))]
async fn secure_write_secret_file(path: &Path, content: &[u8]) -> Result<(), SecretError> {
    tokio::fs::write(path, content).await.map_err(|error| {
        SecretError(format!(
            "Failed to write secret file {}: {error}",
            path.display()
        ))
    })
}

fn parse_secret_file(content: &str) -> Result<String, ()> {
    let trimmed = content.trim();
    if let Ok(secret) = validate_secret(trimmed) {
        return Ok(secret);
    }

    let config = toml::from_str::<SecretConfig>(content).map_err(|_| ())?;
    validate_secret(config.secret_key.trim()).map_err(|_| ())
}

fn validate_secret(secret: &str) -> Result<String, SecretError> {
    hex::decode(secret)
        .map(|_| secret.to_string())
        .map_err(|_| {
            SecretError(
                "The secret is not valid. It should be the key generated by playit setup."
                    .to_string(),
            )
        })
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SecretConfig {
    secret_key: String,
}

fn daemon_error(code: ServiceErrorCode, message: String, retryable: bool) -> ServiceError {
    ServiceError {
        code,
        message,
        retryable,
        details: None,
    }
}

fn agent_disabled_over_limit_message() -> String {
    format!(
        "This account is over the agent limit. The service will retry.\n{}",
        agent_over_limit_guidance()
    )
}

fn setup_error_user_message(error: &SetupError) -> String {
    match error {
        SetupError::FailedToConnect => {
            "Could not connect to playit tunnel servers. Check your internet connection, firewall, VPN, or DNS settings, then restart playit.".to_string()
        }
        SetupError::RequestError(_) => {
            "Could not reach the playit API. Check your internet connection or try again later.".to_string()
        }
        SetupError::ApiError(ApiResponseError::Auth(AuthError::InvalidAgentKey | AuthError::NoLongerValid)) => {
            "The configured playit secret is no longer valid. Run `playit setup` to provision a new secret.".to_string()
        }
        SetupError::ApiError(error) => {
            format!("The playit API rejected the agent startup request: {error}")
        }
        SetupError::ApiFail(ProtoRegisterError::AgentDisabledOverLimit) => {
            agent_disabled_over_limit_message()
        }
        SetupError::ApiFail(_) | SetupError::RawApiFail(_) => {
            "The playit API rejected the agent registration request. Check your account and tunnel configuration, then try again.".to_string()
        }
        SetupError::Timeout(_) => {
            "Timed out while connecting to playit. Check your network/firewall and try again.".to_string()
        }
        SetupError::IoError(error) => {
            format!("Could not open a required network socket: {error}")
        }
        SetupError::AttemptingToAuthWithOldFlow
        | SetupError::FailedToDecodeSignedAgentRegisterHex
        | SetupError::NoResponseFromAuthenticate
        | SetupError::RegisterInvalidSignature
        | SetupError::RegisterUnauthorized => {
            format!("Failed to start the playit agent: {error}")
        }
    }
}

fn is_invalid_agent_secret_error(error: &SetupError) -> bool {
    matches!(
        error,
        SetupError::ApiError(ApiResponseError::Auth(
            AuthError::InvalidAgentKey | AuthError::NoLongerValid
        ))
    )
}

fn is_agent_disabled_over_limit_error(error: &SetupError) -> bool {
    matches!(
        error,
        SetupError::ApiFail(ProtoRegisterError::AgentDisabledOverLimit)
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{DaemonOptions, run_daemon, setup_error_user_message};
    use crate::publisher::RunningSummary;
    use playit_agent_core::agent_control::errors::{SetupError, TimeoutSource};
    use playit_api_client::api::{ApiResponseError, AuthError, ProtoRegisterError};
    use playit_api_client::http_client::HttpClientError;
    use playit_ipc::ipc::IpcClient;
    use playit_ipc::model::{
        AccountStatus as ServiceAccountStatus, AgentLifecycle, AgentState, ServicePhase,
        TunnelState,
    };

    fn unique_test_path(name: &str, extension: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "playitd-{name}-{}-{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            extension,
        ))
    }

    #[test]
    fn setup_error_message_handles_connection_failure() {
        let message = setup_error_user_message(&SetupError::FailedToConnect);

        assert!(message.contains("Could not connect to playit tunnel servers"));
        assert!(message.contains("firewall"));
    }

    #[test]
    fn setup_error_message_handles_timeout() {
        let message = setup_error_user_message(&SetupError::Timeout(TimeoutSource {
            file_name: "test.rs",
            line_no: 1,
        }));

        assert!(message.contains("Timed out while connecting to playit"));
    }

    #[test]
    fn setup_error_message_handles_io_error() {
        let message = setup_error_user_message(&SetupError::IoError(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "address already in use",
        )));

        assert!(message.contains("Could not open a required network socket"));
        assert!(message.contains("address already in use"));
    }

    #[test]
    fn setup_error_message_handles_invalid_secret() {
        let message = setup_error_user_message(&SetupError::ApiError(ApiResponseError::Auth(
            AuthError::NoLongerValid,
        )));

        assert!(message.contains("secret is no longer valid"));
        assert!(message.contains("playit setup"));
    }

    #[test]
    fn setup_error_message_handles_agent_limit() {
        let message = setup_error_user_message(&SetupError::ApiFail(
            ProtoRegisterError::AgentDisabledOverLimit,
        ));

        assert!(message.contains("over the agent limit"));
    }

    #[test]
    fn setup_error_message_handles_generic_api_failures() {
        let message = setup_error_user_message(&SetupError::RawApiFail(
            r#"{"type":"unexpected_future_failure"}"#.to_string(),
        ));

        assert!(message.contains("rejected the agent registration request"));
    }

    #[test]
    fn setup_error_message_handles_api_and_request_failures() {
        let api_message = setup_error_user_message(&SetupError::ApiError(
            ApiResponseError::Validation("invalid request".to_string()),
        ));
        assert!(api_message.contains("rejected the agent startup request"));

        let json_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let request_message = setup_error_user_message(&SetupError::RequestError(
            HttpClientError::SerializeError(json_error),
        ));
        assert!(request_message.contains("Could not reach the playit API"));
    }

    #[test]
    fn setup_error_message_handles_control_protocol_failures() {
        let errors = [
            SetupError::AttemptingToAuthWithOldFlow,
            SetupError::FailedToDecodeSignedAgentRegisterHex,
            SetupError::NoResponseFromAuthenticate,
            SetupError::RegisterInvalidSignature,
            SetupError::RegisterUnauthorized,
        ];

        for error in errors {
            assert!(setup_error_user_message(&error).contains("Failed to start the playit agent"));
        }
    }

    #[test]
    fn running_summary_counts_tunnel_state() {
        let state = AgentState {
            account_status: ServiceAccountStatus::Verified,
            tunnels: vec![
                TunnelState {
                    is_disabled: false,
                    ..TunnelState::default()
                },
                TunnelState {
                    is_disabled: true,
                    ..TunnelState::default()
                },
            ],
            pending_tunnels: vec![Default::default()],
            ..AgentState::default()
        };

        let summary = RunningSummary::from_state(&state);

        assert_eq!(summary.tunnel_count, 2);
        assert_eq!(summary.pending_tunnel_count, 1);
        assert_eq!(summary.disabled_tunnel_count, 1);
        assert_eq!(summary.account_status, "verified");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_log_file_writer_rotates_by_size_and_file_count() {
        use std::io::Write;

        let log_path = unique_test_path("rotating-log", "log");
        for suffix in ["", ".1", ".2", ".3"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", log_path.display()));
        }

        {
            let mut writer =
                crate::logging::windows_log_file_writer_with_limits(&log_path, 64, 2).unwrap();
            for _ in 0..8 {
                writer
                    .write_all(b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\n")
                    .unwrap();
            }
            writer.flush().unwrap();
        }

        let existing_log_files = ["", ".1", ".2", ".3"]
            .iter()
            .filter(|suffix| {
                std::path::PathBuf::from(format!("{}{suffix}", log_path.display())).exists()
            })
            .count();

        assert!(log_path.exists());
        assert!(std::path::PathBuf::from(format!("{}.1", log_path.display())).exists());
        assert!(existing_log_files <= 3);
        assert!(!std::path::PathBuf::from(format!("{}.3", log_path.display())).exists());

        for suffix in ["", ".1", ".2", ".3"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", log_path.display()));
        }
    }

    async fn wait_for_waiting_for_secret(socket_path: &str) -> IpcClient {
        let mut last_lifecycle = None;

        for _ in 0..50 {
            match IpcClient::connect_with_path(socket_path).await {
                Ok(mut client) => match client.lifecycle().await {
                    Ok(AgentLifecycle::WaitingForSecret) => return client,
                    Ok(lifecycle) => {
                        last_lifecycle = Some(format!("{lifecycle:?}"));
                    }
                    Err(error) => {
                        last_lifecycle = Some(format!("lifecycle error: {error}"));
                    }
                },
                Err(error) => {
                    last_lifecycle = Some(format!("connect error: {error}"));
                }
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        panic!(
            "daemon did not report WaitingForSecret over IPC; last observed state: {}",
            last_lifecycle.unwrap_or_else(|| "none".to_string())
        );
    }

    #[tokio::test]
    async fn missing_file_secret_reports_waiting_for_secret() {
        let secret_path = unique_test_path("missing-secret", "toml");
        let socket_path = unique_test_path("missing-secret", "sock")
            .display()
            .to_string();
        let _ = std::fs::remove_file(&secret_path);
        let _ = std::fs::remove_file(&socket_path);

        let daemon_handle = tokio::spawn(run_daemon(DaemonOptions {
            secret: None,
            secret_path: Some(secret_path.clone()),
            socket_path: Some(socket_path.clone()),
            log_path: None,
            platform_docker: false,
            ..DaemonOptions::default()
        }));

        let mut client = wait_for_waiting_for_secret(&socket_path).await;
        let status = client.status().await.unwrap();
        let expected_secret_path = secret_path.display().to_string();

        assert!(matches!(status.phase, ServicePhase::WaitingForSecret));
        assert!(!status.has_secret);
        assert_eq!(
            status.secret_path.as_deref(),
            Some(expected_secret_path.as_str())
        );

        let stop_response = client.stop().await.unwrap();
        assert!(stop_response.accepted);

        let daemon_result = tokio::time::timeout(Duration::from_secs(5), daemon_handle)
            .await
            .expect("daemon did not stop after IPC stop request")
            .expect("daemon task panicked");
        assert!(daemon_result.is_ok());

        let _ = std::fs::remove_file(&secret_path);
        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn file_secret_can_be_provisioned_over_ipc() {
        let secret_path = unique_test_path("provision-secret", "toml");
        let socket_path = unique_test_path("provision-secret", "sock")
            .display()
            .to_string();
        let _ = std::fs::remove_file(&secret_path);
        let _ = std::fs::remove_file(&socket_path);

        let mut daemon_handle = tokio::spawn(run_daemon(DaemonOptions {
            secret: None,
            secret_path: Some(secret_path.clone()),
            socket_path: Some(socket_path.clone()),
            log_path: None,
            platform_docker: false,
            ..DaemonOptions::default()
        }));

        let mut client = wait_for_waiting_for_secret(&socket_path).await;
        let secret = "0123456789abcdef0123456789abcdef";
        let response = client.set_secret(secret).await.unwrap();
        assert!(response.accepted, "{response:?}");

        let content = tokio::fs::read_to_string(&secret_path).await.unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        assert_eq!(parsed["secret_key"].as_str(), Some(secret));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&secret_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        let stop_response = client.stop().await.unwrap();
        assert!(stop_response.accepted);
        if tokio::time::timeout(Duration::from_secs(5), &mut daemon_handle)
            .await
            .is_err()
        {
            daemon_handle.abort();
            let _ = daemon_handle.await;
        }

        let _ = std::fs::remove_file(&secret_path);
        let _ = std::fs::remove_file(&socket_path);
    }
}
