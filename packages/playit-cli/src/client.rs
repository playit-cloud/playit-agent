use std::time::Duration;

use chrono::{DateTime, Utc};
use playit_ipc::ipc::IpcClient;
use playit_ipc::model::{
    AccountStatus as ServiceAccountStatus, AgentLifecycle, AgentState as ServiceAgentState,
    ConnectionStats as ServiceConnectionStats, LogLevel as ServiceLogLevel, ServicePhase,
    ServiceStatus, ServiceUpdate,
};
use playitd::manager::ensure_service_running;
#[cfg(not(target_os = "linux"))]
use playitd::manager::ServiceController;

use crate::ui::log_capture::{LogEntry, LogLevel as UiLogLevel};
use crate::ui::tui_app::{
    AccountStatusInfo, AgentData, ConnectionStats, NoticeInfo, PendingTunnelInfo, TunnelInfo,
};
use crate::ui::UI;
use crate::{CliError, EXE_NAME};

pub async fn run_start_command(
    ui: &mut UI,
    stdout_mode: bool,
    socket_path: Option<&str>,
) -> Result<(), CliError> {
    ui.write_screen("Ensuring installed playitd service is running...")
        .await;
    ensure_service_running(socket_path)
        .await
        .map_err(|e| CliError::ServiceError(format!("Failed to start service: {e}")))?;

    ui.write_screen("Installed playitd service is running").await;
    ui.write_screen("Connecting to playitd...").await;

    let mut client = IpcClient::connect_with_path(socket_path, true)
        .await
        .map_err(|e| CliError::IpcError(format!("Failed to connect to service: {e}")))?;

    let snapshot = client
        .subscribe()
        .await
        .map_err(|e| CliError::IpcError(format!("Failed to subscribe: {e}")))?;

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
                        if stdout_mode {
                            eprintln!("Connection to service lost: {error}");
                        } else {
                            tracing::error!("IPC error: {error}");
                            ui.write_screen(format!("Connection to service lost: {error}")).await;
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

pub async fn run_stop_command(socket_path: Option<&str>) -> Result<(), CliError> {
    if let Ok(mut client) = IpcClient::connect_with_path(socket_path, true).await {
        if let Err(error) = client.stop().await {
            tracing::warn!("Failed to send stop via IPC: {error}");
        } else {
            println!("playitd service stop requested");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Err(error) = playitd::manager::stop_systemd_service() {
            tracing::warn!("Failed to stop via systemctl: {error}");
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        match ServiceController::new() {
            Ok(controller) => {
                if let Err(error) = controller.stop() {
                    tracing::warn!("Failed to stop via service manager: {error}");
                }
            }
            Err(error) => tracing::debug!("Service manager not available: {error}"),
        }
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    if !IpcClient::is_running_with_path(socket_path, true).await {
        println!("playitd service stopped");
    } else {
        println!("playitd service may still be running");
    }

    Ok(())
}

pub async fn run_status_command(socket_path: Option<&str>) -> Result<(), CliError> {
    if !IpcClient::is_running_with_path(socket_path, true).await {
        println!("playitd service is not running");
        return Ok(());
    }

    let mut client = match IpcClient::connect_with_path(socket_path, true).await {
        Ok(client) => client,
        Err(error) => {
            println!("playitd service appears to be running but cannot connect: {error}");
            return Ok(());
        }
    };

    match client.status().await {
        Ok(status) => {
            println!("playitd service status:");
            println!("  Phase: {}", format_service_phase(&status.phase));
            println!("  PID: {}", status.pid);
            println!("  Uptime: {} seconds", status.uptime_secs);
            println!("  Version: {}", status.version);
            println!("  Socket: {}", status.socket_path);
            println!("  Config: {}", status.config_path);
            println!("  Secret configured: {}", status.has_secret);
            println!("  Protocol version: {}", status.protocol.version);
            if !status.protocol.capabilities.is_empty() {
                println!("  Capabilities: {:?}", status.protocol.capabilities);
            }
            if let Some(error) = status.last_error {
                println!("  Last error: {}", error.message);
            }
        }
        Err(error) => println!("Failed to get status: {error}"),
    }

    Ok(())
}

pub async fn provision_service_secret(
    socket_path: Option<&str>,
    secret: &str,
) -> Result<(), CliError> {
    ensure_service_running(socket_path)
        .await
        .map_err(|e| CliError::ServiceError(format!("Failed to start service: {e}")))?;

    let mut client = IpcClient::connect_with_path(socket_path, true)
        .await
        .map_err(|e| CliError::IpcError(format!("Failed to connect to service: {e}")))?;

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
            ui.write_screen("playitd is waiting for a secret to be provisioned").await;
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

    ui.write_screen(format!("playitd status: {}", format_service_phase(&status.phase)))
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
