use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

#[cfg(test)]
use playit_agent_core::agent_control::errors::SetupError;
use playit_agent_core::agent_control::platform::current_platform;
use playit_agent_core::agent_control::version::{help_register_version, register_platform};
use playit_agent_core::gateway::Platform;
use playit_agent_core::network::tcp::tcp_settings::TcpSettings;
use playit_agent_core::network::udp::udp_settings::UdpSettings;
use playit_agent_core::playit_agent::ServiceExit;
use playit_agent_core::playit_agent::{ControlSettings, EngineLimits, PlayitAgentSettings};
use playit_ipc::ipc::{IpcError, get_default_socket_path, protocol_info};
use playit_ipc::model::ServiceUpdate;
use playit_model::{AppConfig, RawAppConfig, ServiceInfo};
#[cfg(test)]
use playit_runtime::setup_error_message;
use playit_runtime::{
    AppSupervisor, Clock, FileSecretStore, InlineSecretStore, IpcPort, SecretStore, ServiceChild,
    SupervisedEnginePort, SupervisorConfig, SupervisorError, SupervisorPolicy, SystemClock,
};
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

use crate::ipc_server::{IpcServer, IpcServerConfig};
use crate::logging::{IpcBroadcastLayer, log_rate_limit_filter};

pub const DEFAULT_VARIANT_ID: &str = "308943e8-faef-4835-a2ba-270351f72aa3";
#[cfg(target_os = "windows")]
const WINDOWS_LOG_MAX_FILE_SIZE_BYTES: u64 = 5 * 1024 * 1024;
#[cfg(target_os = "windows")]
const WINDOWS_LOG_MAX_TOTAL_FILES: usize = 3;
#[cfg(target_os = "windows")]
const WINDOWS_LOG_MAX_ROTATED_FILES: usize = WINDOWS_LOG_MAX_TOTAL_FILES - 1;

#[derive(Clone)]
pub struct DaemonOptions {
    pub secret: Option<String>,
    pub secret_path: Option<PathBuf>,
    pub socket_path: Option<String>,
    pub log_path: Option<PathBuf>,
    pub platform_docker: bool,
    pub version: VersionDetails,
}

impl std::fmt::Debug for DaemonOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DaemonOptions")
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .field("secret_path", &self.secret_path)
            .field("socket_path", &self.socket_path)
            .field("log_path", &self.log_path)
            .field("platform_docker", &self.platform_docker)
            .field("version", &self.version)
            .finish()
    }
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            secret: None,
            secret_path: Some(playit_platform::default_secret_path()),
            socket_path: None,
            log_path: None,
            platform_docker: false,
            version: VersionDetails::from_cargo_package()
                .expect("Cargo package version must be a valid semver triplet"),
        }
    }
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
            variant_id: variant_id.to_owned(),
            major,
            minor,
            patch,
        })
    }

    pub fn apply_overrides(&mut self, overrides: VersionOverrideFile) {
        if let Some(value) = overrides.variant_id {
            self.variant_id = value;
        }
        if let Some(value) = overrides.major {
            self.major = value;
        }
        if let Some(value) = overrides.minor {
            self.minor = value;
        }
        if let Some(value) = overrides.patch {
            self.patch = value;
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

pub async fn load_version_overrides(path: &Path) -> Result<VersionOverrideFile, String> {
    let content = tokio::fs::read_to_string(path).await.map_err(|error| {
        format!(
            "Failed to read version override file {}: {error}",
            path.display()
        )
    })?;
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") => serde_json::from_str(&content)
            .map_err(|error| format!("Invalid JSON in {}: {error}", path.display())),
        Some("yaml") | Some("yml") => serde_yml::from_str(&content)
            .map_err(|error| format!("Invalid YAML in {}: {error}", path.display())),
        _ => Err(format!(
            "Unsupported version override file format for {}. Use .json, .yaml, or .yml",
            path.display()
        )),
    }
}

#[derive(Debug)]
pub enum DaemonError {
    Ipc(IpcError),
    Setup(String),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ipc(error) => write!(formatter, "IPC error: {error}"),
            Self::Setup(error) => write!(formatter, "Setup error: {error}"),
        }
    }
}

impl std::error::Error for DaemonError {}

impl From<IpcError> for DaemonError {
    fn from(error: IpcError) -> Self {
        Self::Ipc(error)
    }
}

pub async fn run_daemon(options: DaemonOptions) -> Result<(), DaemonError> {
    let clock = Arc::new(SystemClock);
    let start_time = clock.now_millis();
    let version = options.version.version_string();
    let platform = if options.platform_docker {
        Platform::Docker
    } else {
        current_platform()
    };
    let socket_path = options
        .socket_path
        .clone()
        .unwrap_or_else(|| get_default_socket_path().to_owned());
    let secret_path = options
        .secret_path
        .clone()
        .unwrap_or_else(playit_platform::default_secret_path);
    let app_config = validate_runtime_config(
        api_base(),
        if options.secret.is_some() {
            "inline-secret".to_owned()
        } else {
            secret_path.display().to_string()
        },
        socket_path.clone(),
    )?;
    let secrets: Arc<dyn SecretStore> = match options.secret.clone() {
        Some(secret) => Arc::new(InlineSecretStore::new(secret)),
        None => Arc::new(FileSecretStore::new(secret_path)),
    };

    let (log_tx, _) = broadcast::channel::<ServiceUpdate>(256);
    let log_filter =
        EnvFilter::try_from_env("PLAYIT_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let use_ansi = matches!(platform, Platform::Linux | Platform::Docker);
    let _log_guard = init_tracing(
        log_filter,
        use_ansi,
        log_tx.clone(),
        options.log_path.as_deref(),
    )
    .map_err(DaemonError::Setup)?;
    register_platform(platform);
    let _ = help_register_version(&version, &options.version.variant_id);
    tracing::info!(socket_path = %socket_path, secret_path = ?secrets.path(), version = %version, "starting playitd");

    let policy = SupervisorPolicy {
        refresh_retry: app_config.control.retry_delay,
        shutdown_deadline: app_config.shutdown.drain_timeout,
        gateway_timeout: app_config.control.connect_timeout,
        ..SupervisorPolicy::default()
    };
    let config = SupervisorConfig {
        api_base: app_config.api_base.as_str().to_owned(),
        version: version.clone(),
        start_time,
        service: ServiceInfo {
            process_id: Some(std::process::id()),
            uptime_secs: 0,
            started_at_millis: start_time,
            version: Some(version),
            ipc_protocol: protocol_info().ipc_version,
            ipc_endpoint: Some(socket_path),
            secret_location: secrets.path().map(|path| path.display().to_string()),
            has_secret: false,
        },
        policy,
    };
    let limits = EngineLimits {
        shutdown_deadline: app_config.shutdown.drain_timeout,
        ..EngineLimits::default()
    };
    let engine_settings = PlayitAgentSettings {
        control_settings: ControlSettings {
            connect_timeout: app_config.control.connect_timeout,
            retry_delay: app_config.control.retry_delay,
            event_queue_capacity: app_config.control.event_queue_capacity.get(),
        },
        tcp_settings: TcpSettings {
            new_client_ratelimit: app_config.tcp.per_second.get(),
            new_client_ratelimit_burst: app_config.tcp.burst.get(),
            queue_capacity: app_config.tcp.queue_capacity.get(),
            ..TcpSettings::default()
        },
        udp_settings: UdpSettings {
            new_client_ratelimit: app_config.udp.per_second.get(),
            new_client_ratelimit_burst: app_config.udp.burst.get(),
            queue_capacity: app_config.udp.queue_capacity.get(),
        },
        limits,
    };
    let (mut supervisor, commands, snapshots) = AppSupervisor::new(
        config,
        secrets.clone(),
        Arc::new(SupervisedEnginePort::new(engine_settings)),
        clock,
    );

    let ipc_cancel = CancellationToken::new();
    let ipc = Arc::new(
        IpcServer::new_with_sender(
            IpcServerConfig {
                socket_path: options.socket_path,
                snapshots,
                secret_path: secrets.path().map(PathBuf::from),
                commands: Some(commands),
            },
            ipc_cancel.clone(),
            log_tx,
        )
        .await?,
    );
    let listener = ipc.bind_listener().await?;
    supervisor.install_ipc(Box::new(IpcService {
        server: ipc,
        listener: Some(listener),
        cancel: ipc_cancel,
    }));

    let process_shutdown = CancellationToken::new();
    let signal_shutdown = process_shutdown.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_shutdown.cancel();
        }
    });
    let result = supervisor.run(process_shutdown).await;
    signal.abort();
    let _ = signal.await;
    result.map_err(map_supervisor_error)
}

fn api_base() -> String {
    dotenv::var("API_BASE").unwrap_or_else(|_| "https://api.playit.gg".to_owned())
}

fn validate_runtime_config(
    api_base: String,
    secret_location: String,
    ipc_location: String,
) -> Result<AppConfig, DaemonError> {
    AppConfig::validate(RawAppConfig {
        api_base,
        secret_location,
        ipc_location,
        ..RawAppConfig::default()
    })
    .map_err(|error| DaemonError::Setup(format!("invalid runtime configuration: {error:?}")))
}

struct IpcService {
    server: Arc<IpcServer>,
    listener: Option<interprocess::local_socket::tokio::Listener>,
    cancel: CancellationToken,
}

impl IpcPort for IpcService {
    fn start(mut self: Box<Self>) -> ServiceChild {
        let listener = self.listener.take().expect("IPC listener starts once");
        let server = self.server.clone();
        let exit = tokio::spawn(async move {
            match server.run(listener).await {
                Ok(()) => ServiceExit::Completed,
                Err(error) => ServiceExit::Failed(error.to_string()),
            }
        });
        ServiceChild::new(self.cancel.clone(), exit)
    }
}

fn map_supervisor_error(error: SupervisorError) -> DaemonError {
    let failure = error.failure();
    let detail = failure.detail.unwrap_or_else(|| format!("{error:?}"));
    DaemonError::Setup(format!("[{}] {detail}", failure.problem.code.as_str()))
}

fn parse_version_part(part: &str) -> Result<u32, String> {
    u32::from_str(part).map_err(|error| format!("Invalid version component {part}: {error}"))
}

fn init_tracing(
    log_filter: EnvFilter,
    use_ansi: bool,
    event_tx: broadcast::Sender<ServiceUpdate>,
    log_path: Option<&Path>,
) -> Result<Option<WorkerGuard>, String> {
    match log_path {
        Some(path) => {
            let writer = log_file_writer(path)?;
            let (non_blocking, guard) = tracing_appender::non_blocking(writer);
            tracing_subscriber::registry()
                .with(log_filter)
                .with(
                    IpcBroadcastLayer::new(event_tx)
                        .and_then(
                            tracing_subscriber::fmt::layer()
                                .with_ansi(use_ansi)
                                .with_writer(non_blocking),
                        )
                        .with_filter(log_rate_limit_filter()),
                )
                .init();
            Ok(Some(guard))
        }
        None => {
            tracing_subscriber::registry()
                .with(log_filter)
                .with(
                    IpcBroadcastLayer::new(event_tx)
                        .and_then(
                            tracing_subscriber::fmt::layer()
                                .with_ansi(use_ansi)
                                .with_writer(std::io::stderr),
                        )
                        .with_filter(log_rate_limit_filter()),
                )
                .init();
            Ok(None)
        }
    }
}

#[cfg(target_os = "windows")]
fn log_file_writer(path: &Path) -> Result<tracing_rolling_file::RollingFileAppenderBase, String> {
    create_log_parent_dir(path)?;
    Ok(tracing_rolling_file::RollingFileAppenderBase::builder()
        .filename(path.display().to_string())
        .max_filecount(WINDOWS_LOG_MAX_ROTATED_FILES)
        .condition_max_file_size(WINDOWS_LOG_MAX_FILE_SIZE_BYTES)
        .build()
        .map_err(|error| {
            format!(
                "Failed to create log file writer {}: {error}",
                path.display()
            )
        })?)
}

#[cfg(not(target_os = "windows"))]
fn log_file_writer(path: &Path) -> Result<tracing_appender::rolling::RollingFileAppender, String> {
    create_log_parent_dir(path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|file| file.to_str())
        .ok_or_else(|| format!("Invalid --log-path {}", path.display()))?;
    Ok(tracing_appender::rolling::never(parent, file_name))
}

fn create_log_parent_dir(path: &Path) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "Failed to create log directory {}: {error}",
            parent.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parser_ignores_prerelease_suffix() {
        let version = VersionDetails::from_version_string("1.2.3-beta.1", "fixture").unwrap();
        assert_eq!(version.version_string(), "1.2.3");
        assert_eq!(version.variant_id, "fixture");
    }

    #[test]
    fn setup_error_has_network_guidance() {
        let message = setup_error_message(&SetupError::FailedToConnect);
        assert!(message.contains("firewall"));
    }

    #[test]
    fn production_config_is_validated_before_runtime_construction() {
        let error = validate_runtime_config(
            "api.playit.gg".to_owned(),
            "playit.toml".to_owned(),
            "playit.sock".to_owned(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid runtime configuration"));
    }
}
