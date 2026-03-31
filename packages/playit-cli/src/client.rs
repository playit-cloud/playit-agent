use std::time::Duration;

use chrono::{DateTime, Utc};
use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playit_ipc::model::{
    AccountStatus as ServiceAccountStatus, AgentLifecycle, AgentState as ServiceAgentState,
    ConnectionStats as ServiceConnectionStats, LogLevel as ServiceLogLevel, ServicePhase,
    ServiceStatus, ServiceUpdate,
};
use playitd::manager::{ensure_installed_service_running, stop_installed_service};

use crate::ui::UI;
use crate::ui::log_capture::{LogEntry, LogLevel as UiLogLevel};
use crate::ui::tui_app::{
    AccountStatusInfo, AgentData, ConnectionStats, NoticeInfo, PendingTunnelInfo, TunnelInfo,
};
use crate::{CliError, EXE_NAME, run_setup_flow};

enum AttachErrorContext {
    Standard,
    AutoCommand {
        start_attempt_failed: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum CliTarget {
    InstalledService,
    ExplicitSocket(String),
}

impl CliTarget {
    pub fn from_socket_path(socket_path: Option<String>) -> Self {
        match socket_path {
            Some(socket_path) => Self::ExplicitSocket(socket_path),
            None => Self::InstalledService,
        }
    }

    fn socket_path(&self) -> &str {
        match self {
            Self::InstalledService => get_default_socket_path(),
            Self::ExplicitSocket(path) => path.as_str(),
        }
    }
}

pub async fn run_attach_command(
    ui: &mut UI,
    stdout_mode: bool,
    target: &CliTarget,
) -> Result<(), CliError> {
    run_attach_command_with_context(ui, stdout_mode, target, AttachErrorContext::Standard).await
}

pub async fn run_auto_command(ui: &mut UI, target: &CliTarget) -> Result<(), CliError> {
    let start_attempt_failed = match target {
        CliTarget::InstalledService => ensure_installed_service_running()
            .await
            .err()
            .map(|error| error.to_string()),
        CliTarget::ExplicitSocket(_) => None,
    };

    let mut client = connect_target(target).await.map_err(|_| {
        initial_attach_error(
            target,
            &AttachErrorContext::AutoCommand {
                start_attempt_failed: start_attempt_failed.clone(),
            },
        )
    })?;

    match client
        .lifecycle()
        .await
        .map_err(|e| CliError::IpcError(format!("Failed to read playitd lifecycle: {e}")))?
    {
        AgentLifecycle::Running(_) | AgentLifecycle::Starting => {}
        AgentLifecycle::WaitingForSecret => {
            run_setup_flow(ui, target).await?;
        }
        AgentLifecycle::HasInvalidSecret(error) => {
            let should_reset = ui
                .yn_question(
                    format!(
                        "playitd has an invalid secret configuration: {}.\nReset the secret and run setup again?",
                        error.message
                    ),
                    Some(false),
                )
                .await?;

            if !should_reset {
                return Err(CliError::ServiceError(
                    "playitd has an invalid secret configuration. Run `playit reset` or rerun `playit` and confirm the reset prompt to reclaim this agent."
                        .to_string(),
                ));
            }

            reset_service_secret_for_setup(target).await?;
            wait_for_service_waiting_for_secret(target).await?;
            run_setup_flow(ui, target).await?;
        }
        AgentLifecycle::Stopping => {
            return Err(CliError::ServiceError(
                "playitd is stopping and cannot be auto-attached right now".to_string(),
            ));
        }
        AgentLifecycle::Error(error) => {
            return Err(CliError::ServiceError(format!(
                "playitd reported an error and cannot continue auto mode: {}",
                error.message
            )));
        }
    }

    run_attach_command_with_context(
        ui,
        false,
        target,
        AttachErrorContext::AutoCommand {
            start_attempt_failed,
        },
    )
    .await
}

async fn reset_service_secret_for_setup(target: &CliTarget) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let response = client
        .reset_secret()
        .await
        .map_err(|e| CliError::IpcError(format!("Failed to reset secret: {e}")))?;

    if !response.accepted {
        return Err(CliError::IpcError(response.message.unwrap_or_else(|| {
            "playitd rejected the reset request".to_string()
        })));
    }

    Ok(())
}

async fn wait_for_service_waiting_for_secret(target: &CliTarget) -> Result<(), CliError> {
    for _ in 0..50 {
        let mut client = match connect_target(target).await {
            Ok(client) => client,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let lifecycle = client
            .lifecycle()
            .await
            .map_err(|e| CliError::IpcError(format!("Failed to read playitd lifecycle: {e}")))?;

        match lifecycle {
            AgentLifecycle::WaitingForSecret => return Ok(()),
            AgentLifecycle::HasInvalidSecret(_)
            | AgentLifecycle::Running(_)
            | AgentLifecycle::Starting => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            AgentLifecycle::Stopping => {
                return Err(CliError::ServiceError(
                    "playitd is stopping and did not become ready for setup after reset"
                        .to_string(),
                ));
            }
            AgentLifecycle::Error(error) => {
                return Err(CliError::ServiceError(format!(
                    "playitd reported an error after reset: {}",
                    error.message
                )));
            }
        }
    }

    Err(CliError::ServiceError(
        "Timed out waiting for playitd to become ready for setup after reset".to_string(),
    ))
}

async fn run_attach_command_with_context(
    ui: &mut UI,
    stdout_mode: bool,
    target: &CliTarget,
    error_context: AttachErrorContext,
) -> Result<(), CliError> {
    ui.write_screen("Connecting to playitd...").await;
    let mut client = connect_target(target)
        .await
        .map_err(|_| initial_attach_error(target, &error_context))?;

    let snapshot = client
        .subscribe()
        .await
        .map_err(|_| initial_attach_error(target, &error_context))?;

    if !stdout_mode {
        apply_status(ui, snapshot.snapshot.status.clone(), false).await;
        apply_lifecycle(ui, snapshot.snapshot.lifecycle.clone()).await;
        ui.update_stats(snapshot.snapshot.stats.into());
    }

    loop {
        tokio::select! {
            update_result = client.recv_update() => {
                match update_result {
                    Ok(update) => apply_update(ui, update, stdout_mode).await,
                    Err(error) => {
                        let message = attach_lost_message(target, &error.to_string());
                        if stdout_mode {
                            eprintln!("{message}");
                        } else {
                            tracing::error!("IPC error: {error}");
                            ui.write_screen(message).await;
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if !stdout_mode && ui.is_tui() {
                    match ui.tick_tui() {
                        Ok(true) => {}
                        Ok(false) => {
                            ui.shutdown_tui()?;
                            println!("Detached from service. Service continues running in background.");
                            println!("Use '{} stop' to stop the service.", *EXE_NAME);
                            break;
                        }
                        Err(error) => {
                            ui.shutdown_tui()?;
                            return Err(error);
                        }
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                if !stdout_mode && ui.is_tui() {
                    ui.shutdown_tui()?;
                }
                println!("\nDetached from service. Service continues running in background.");
                println!("Use '{} stop' to stop the service.", *EXE_NAME);
                break;
            }
        }
    }

    Ok(())
}

pub async fn run_start_command(target: &CliTarget) -> Result<(), CliError> {
    if let CliTarget::ExplicitSocket(path) = target {
        return Err(CliError::ServiceError(format!(
            "The start command only manages the installed playitd service and cannot be used with --socket-path ({path})."
        )));
    }

    ensure_installed_service_running()
        .await
        .map_err(|e| CliError::ServiceError(format!("Failed to start service: {e}")))?;

    println!("playitd service started");
    println!("Run \"playit attach\" to see the playit program.");
    Ok(())
}

pub async fn run_stop_command(target: &CliTarget) -> Result<(), CliError> {
    match target {
        CliTarget::InstalledService => {
            match connect_target(target).await {
                Ok(mut client) => match client.stop().await {
                    Ok(response) if response.accepted => {
                        println!("playitd service stop requested");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    Ok(response) => {
                        tracing::warn!(
                            "playitd rejected stop request: {}",
                            response
                                .message
                                .unwrap_or_else(|| "service rejected stop request".to_string())
                        );
                    }
                    Err(error) => {
                        tracing::warn!("Failed to send stop via IPC: {error}");
                        eprintln!(
                            "Could not reach playitd over IPC, attempting to stop the installed service directly."
                        );
                    }
                },
                Err(error) => {
                    tracing::warn!("Failed to connect to installed service over IPC: {error}");
                    eprintln!(
                        "Could not reach playitd over IPC, attempting to stop the installed service directly."
                    );
                }
            }

            if let Err(error) = stop_installed_service() {
                tracing::warn!("Failed to stop installed service: {error}");
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
            if !IpcClient::is_running(get_default_socket_path()).await {
                println!("playitd service stopped");
            } else {
                println!("playitd service may still be running");
            }

            Ok(())
        }
        CliTarget::ExplicitSocket(path) => {
            let mut client = connect_target(target).await?;
            let response = client
                .stop()
                .await
                .map_err(|e| CliError::IpcError(format!("Failed to stop daemon at {path}: {e}")))?;

            if !response.accepted {
                return Err(CliError::IpcError(response.message.unwrap_or_else(|| {
                    format!("playitd rejected stop request for {path}")
                })));
            }

            println!("playitd stop requested for socket {path}");
            tokio::time::sleep(Duration::from_secs(1)).await;

            if !IpcClient::is_running(path.as_str()).await {
                println!("playitd daemon stopped");
            } else {
                println!("playitd daemon may still be running");
            }

            Ok(())
        }
    }
}

pub async fn run_status_command(target: &CliTarget) -> Result<(), CliError> {
    if !IpcClient::is_running(target.socket_path()).await {
        match target {
            CliTarget::InstalledService => println!("playitd service is not running"),
            CliTarget::ExplicitSocket(path) => {
                println!("playitd daemon is not reachable at socket {path}")
            }
        }
        return Ok(());
    }

    let mut client = connect_target(target).await?;

    match client.status().await {
        Ok(status) => {
            match target {
                CliTarget::InstalledService => println!("playitd service status:"),
                CliTarget::ExplicitSocket(path) => {
                    println!("playitd daemon status for socket {path}:")
                }
            }
            println!("  Phase: {}", format_service_phase(&status.phase));
            println!("  PID: {}", status.pid);
            println!("  Uptime: {} seconds", status.uptime_secs);
            println!("  Version: {}", status.version);
            println!("  Socket: {}", status.socket_path);
            match &status.secret_path {
                Some(secret_path) => println!("  Secret path: {}", secret_path),
                None => println!("  Secret path: <inline secret>"),
            }
            println!("  Secret configured: {}", status.has_secret);
            println!("  Protocol version: {}", status.protocol.version);
            if !status.protocol.capabilities.is_empty() {
                println!("  Capabilities: {:?}", status.protocol.capabilities);
            }
            if let Some(error) = status.last_error {
                println!("  Last error: {}", error.message);
            }
        }
        Err(_) => return Err(ipc_connection_error()),
    }

    Ok(())
}

pub async fn ensure_service_waiting_for_secret(target: &CliTarget) -> Result<(), CliError> {
    if matches!(target, CliTarget::InstalledService) {
        ensure_installed_service_running()
            .await
            .map_err(|e| CliError::ServiceError(format!("Failed to start service: {e}")))?;
    }

    let mut client = connect_target(target).await?;
    let lifecycle = client
        .lifecycle()
        .await
        .map_err(|e| CliError::IpcError(format!("Failed to read playitd lifecycle: {e}")))?;

    match lifecycle {
        AgentLifecycle::WaitingForSecret => Ok(()),
        AgentLifecycle::HasInvalidSecret(error) => Err(CliError::ServiceError(format!(
            "playitd is not waiting for setup because it has an invalid secret configuration: {}. Reset the daemon secret first.",
            error.message
        ))),
        AgentLifecycle::Starting => Err(CliError::ServiceError(
            "playitd is starting and is not waiting for setup".to_string(),
        )),
        AgentLifecycle::Running(_) => Err(CliError::ServiceError(
            "playitd already has a configured secret and is not waiting for setup. Run `playit reset` before claiming a new agent."
                .to_string(),
        )),
        AgentLifecycle::Stopping => Err(CliError::ServiceError(
            "playitd is stopping and is not waiting for setup".to_string(),
        )),
        AgentLifecycle::Error(error) => Err(CliError::ServiceError(format!(
            "playitd reported an error and is not waiting for setup: {}",
            error.message
        ))),
    }
}

pub async fn provision_service_secret(target: &CliTarget, secret: &str) -> Result<(), CliError> {
    if matches!(target, CliTarget::InstalledService) {
        ensure_installed_service_running()
            .await
            .map_err(|e| CliError::ServiceError(format!("Failed to start service: {e}")))?;
    }

    let mut client = connect_target(target).await?;

    let response = client
        .set_secret(secret)
        .await
        .map_err(|e| CliError::IpcError(format!("Failed to provision secret: {e}")))?;

    if !response.accepted {
        return Err(CliError::IpcError(
            response
                .message
                .unwrap_or_else(|| "playitd rejected the secret".to_string()),
        ));
    }

    Ok(())
}

pub async fn run_reset_command(target: &CliTarget) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let reset_response = client
        .reset_secret()
        .await
        .map_err(|e| CliError::IpcError(format!("Failed to reset secret: {e}")))?;

    if !reset_response.accepted {
        return Err(CliError::IpcError(reset_response.message.unwrap_or_else(
            || "playitd rejected the reset request".to_string(),
        )));
    }

    let stop_response = client.stop().await.map_err(|e| {
        CliError::IpcError(format!("Secret was reset, but failed to stop playitd: {e}"))
    })?;

    if !stop_response.accepted {
        return Err(CliError::IpcError(stop_response.message.unwrap_or_else(
            || "Secret was reset, but playitd rejected the stop request".to_string(),
        )));
    }

    let reset_message = reset_response
        .message
        .unwrap_or_else(|| "playitd reset the secret file".to_string());
    let stop_message = stop_response
        .message
        .unwrap_or_else(|| "shutdown requested".to_string());

    println!("{reset_message}");
    println!("playitd stop requested: {stop_message}");
    Ok(())
}

pub async fn run_secret_path_command(target: &CliTarget) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let response = client
        .get_secret_path()
        .await
        .map_err(|e| CliError::IpcError(format!("Failed to read secret path: {e}")))?;

    let Some(secret_path) = response.secret_path else {
        return Err(CliError::IpcError(
            "playitd is using an inline --secret, so no secret file path is available".to_string(),
        ));
    };

    println!("{secret_path}");
    Ok(())
}

pub async fn run_account_login_url_command(target: &CliTarget) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let response = client
        .get_account_login_url()
        .await
        .map_err(|e| CliError::IpcError(format!("Failed to create account login URL: {e}")))?;

    println!("{}", response.login_url);
    Ok(())
}

async fn connect_target(target: &CliTarget) -> Result<IpcClient, CliError> {
    IpcClient::connect_with_path(target.socket_path())
        .await
        .map_err(|_| ipc_connection_error())
}

fn ipc_connection_error() -> CliError {
    CliError::IpcError(
        "Failed to connect to playitd over IPC. Start playitd and try again.".to_string(),
    )
}

fn initial_attach_error(target: &CliTarget, error_context: &AttachErrorContext) -> CliError {
    match error_context {
        AttachErrorContext::Standard => ipc_connection_error(),
        AttachErrorContext::AutoCommand {
            start_attempt_failed,
        } => auto_attach_error(target, start_attempt_failed.as_deref()),
    }
}

fn auto_attach_error(target: &CliTarget, start_attempt_failed: Option<&str>) -> CliError {
    match target {
        CliTarget::InstalledService => match start_attempt_failed {
            Some(error) => CliError::IpcError(format!(
                "Failed to connect to playitd over IPC. playit-cli auto mode also tried starting playitd first, but that start attempt failed: {error}"
            )),
            None => CliError::IpcError(
                "Failed to connect to playitd over IPC. playit-cli auto mode prepared the service first, but it is still not reachable."
                    .to_string(),
            ),
        },
        CliTarget::ExplicitSocket(_) => ipc_connection_error(),
    }
}

fn attach_lost_message(target: &CliTarget, error: &str) -> String {
    match target {
        CliTarget::InstalledService => {
            format!("Connection to playitd lost: {error}. Run \"playit attach\" to reconnect.")
        }
        CliTarget::ExplicitSocket(path) => format!(
            "Connection to playitd lost: {error}. Reattach with \"playit attach --socket-path {}\" once the daemon is reachable again.",
            path
        ),
    }
}

async fn apply_update(ui: &mut UI, update: ServiceUpdate, stdout_mode: bool) {
    match update {
        ServiceUpdate::Lifecycle(state) => {
            if !stdout_mode {
                apply_lifecycle(ui, state).await;
            }
        }
        ServiceUpdate::Status(status) => apply_status(ui, status, stdout_mode).await,
        ServiceUpdate::Stats(stats) => {
            if !stdout_mode {
                ui.update_stats(stats.into());
            }
        }
        ServiceUpdate::Log(entry) => {
            if stdout_mode {
                println!(
                    "{} {:>5} {}: {}",
                    format_timestamp_millis(entry.timestamp),
                    format_log_level(&entry.level),
                    entry.target,
                    entry.message
                );
            } else if let Some(log_capture) = ui.log_capture() {
                let level = match entry.level {
                    ServiceLogLevel::Error => UiLogLevel::Error,
                    ServiceLogLevel::Warn => UiLogLevel::Warn,
                    ServiceLogLevel::Info => UiLogLevel::Info,
                    ServiceLogLevel::Debug => UiLogLevel::Debug,
                    ServiceLogLevel::Trace => UiLogLevel::Trace,
                };

                log_capture.push(LogEntry {
                    level,
                    target: entry.target,
                    message: entry.message,
                    timestamp: entry.timestamp,
                });
            }
        }
    }
}

async fn apply_lifecycle(ui: &mut UI, lifecycle: AgentLifecycle) {
    match lifecycle {
        AgentLifecycle::Running(state) => ui.update_agent_data(state.into()),
        AgentLifecycle::WaitingForSecret => {
            ui.write_screen("playitd is waiting for a secret to be provisioned")
                .await;
        }
        AgentLifecycle::HasInvalidSecret(error) => {
            ui.write_screen(format!(
                "playitd has an invalid secret configuration: {}",
                error.message
            ))
            .await;
        }
        AgentLifecycle::Starting => {
            ui.write_screen("playitd is starting the agent").await;
        }
        AgentLifecycle::Stopping => {
            ui.write_screen("playitd is stopping").await;
        }
        AgentLifecycle::Error(error) => {
            ui.write_screen(format!("playitd reported an error: {}", error.message))
                .await;
        }
    }
}

async fn apply_status(ui: &mut UI, status: ServiceStatus, stdout_mode: bool) {
    if stdout_mode {
        return;
    }

    if let Some(error) = status.last_error {
        ui.write_screen(format!(
            "playitd status: {} ({})",
            format_service_phase(&status.phase),
            error.message
        ))
        .await;
        return;
    }

    ui.write_screen(format!(
        "playitd status: {}",
        format_service_phase(&status.phase)
    ))
    .await;
}

fn format_timestamp_millis(millis: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(millis as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
        .unwrap_or_else(|| format!("{millis}ms"))
}

fn format_service_phase(phase: &ServicePhase) -> &'static str {
    match phase {
        ServicePhase::WaitingForSecret => "waiting_for_secret",
        ServicePhase::HasInvalidSecret => "has_invalid_secret",
        ServicePhase::Starting => "starting",
        ServicePhase::Running => "running",
        ServicePhase::Stopping => "stopping",
        ServicePhase::Error => "error",
    }
}

fn format_log_level(level: &ServiceLogLevel) -> &'static str {
    match level {
        ServiceLogLevel::Trace => "TRACE",
        ServiceLogLevel::Debug => "DEBUG",
        ServiceLogLevel::Info => "INFO",
        ServiceLogLevel::Warn => "WARN",
        ServiceLogLevel::Error => "ERROR",
    }
}

impl From<ServiceAgentState> for AgentData {
    fn from(data: ServiceAgentState) -> Self {
        Self {
            version: data.version,
            tunnels: data
                .tunnels
                .into_iter()
                .map(|t| TunnelInfo {
                    display_address: t.display_address,
                    destination: t.destination,
                    is_disabled: t.is_disabled,
                    disabled_reason: t.disabled_reason,
                })
                .collect(),
            pending_tunnels: data
                .pending_tunnels
                .into_iter()
                .map(|p| PendingTunnelInfo {
                    id: p.id,
                    status_msg: p.status_msg,
                })
                .collect(),
            notices: data
                .notices
                .into_iter()
                .map(|n| NoticeInfo {
                    priority: n.priority,
                    message: n.message,
                    resolve_link: n.resolve_link,
                })
                .collect(),
            account_status: match data.account_status {
                ServiceAccountStatus::Guest => AccountStatusInfo::Guest,
                ServiceAccountStatus::EmailNotVerified => AccountStatusInfo::EmailNotVerified,
                ServiceAccountStatus::Verified => AccountStatusInfo::Verified,
                ServiceAccountStatus::Unknown => AccountStatusInfo::Unknown,
            },
            agent_id: data.agent_id,
            login_link: data.login_link,
            start_time: data.start_time,
        }
    }
}

impl From<ServiceConnectionStats> for ConnectionStats {
    fn from(stats: ServiceConnectionStats) -> Self {
        Self {
            bytes_in: stats.bytes_in,
            bytes_out: stats.bytes_out,
            active_tcp: stats.active_tcp,
            active_udp: stats.active_udp,
        }
    }
}
