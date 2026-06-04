use std::time::Duration;

use chrono::{DateTime, Utc};
use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playit_ipc::model::{
    AgentLifecycle, LogLevel as ServiceLogLevel, ServicePhase, ServiceUpdate, SubscribeResponse,
};

#[cfg(target_os = "linux")]
use crate::linux;
use crate::service::{
    InstalledServiceStartState, InstalledServiceStopState, ServiceManagerMode,
    ensure_installed_service_running_for_cli, stop_installed_service_for_cli,
};
use crate::ui::{ConnectionStats, ConsoleUi, TuiApp};
use crate::{CliError, run_setup_flow};

const ACCOUNT_AGENTS_URL: &str = "https://playit.gg/account/agents";
const ACCOUNT_UPGRADE_URL: &str = "https://playit.gg/account/upgrade";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachMode {
    Interactive,
    Stdout,
}

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

    pub fn socket_path(&self) -> &str {
        match self {
            Self::InstalledService => get_default_socket_path(),
            Self::ExplicitSocket(path) => path.as_str(),
        }
    }
}

pub async fn run_attach_command(target: &CliTarget, mode: AttachMode) -> Result<(), CliError> {
    run_attach_command_with_context(target, mode, AttachErrorContext::Standard).await
}

pub async fn run_auto_command(
    console: &mut ConsoleUi,
    target: &CliTarget,
    attach_mode: AttachMode,
    service_manager: ServiceManagerMode,
) -> Result<(), CliError> {
    let start_attempt_failed = match target {
        CliTarget::InstalledService => {
            ensure_installed_service_running_for_cli(Some(console), service_manager)
                .await
                .err()
                .map(|error| error.to_string())
        }
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

    match wait_for_auto_lifecycle(&mut client).await? {
        AgentLifecycle::Running(_) => {}
        AgentLifecycle::WaitingForSecret => {
            run_setup_flow(console, target, service_manager).await?;
        }
        AgentLifecycle::HasInvalidSecret(error) => {
            let should_reset = console
                .yn_question(
                    format!(
                        "The playit service has an invalid secret: {}.\nReset it now and run setup again?",
                        error.message
                    ),
                    Some(false),
                )
                .await?;

            if !should_reset {
                return Err(CliError::ServiceError(
                    "The playit service has an invalid secret. Run `playit reset`, then run `playit` again to set up this agent."
                        .to_string(),
                ));
            }

            reset_service_secret_for_setup(target).await?;
            wait_for_service_waiting_for_secret(target).await?;
            run_setup_flow(console, target, service_manager).await?;
        }
        AgentLifecycle::DisabledOverLimit(_) => {
            return Err(CliError::ServiceError(format!(
                "{}\n{}",
                agent_over_limit_title(),
                agent_over_limit_guidance()
            )));
        }
        AgentLifecycle::Starting => {
            return Err(CliError::ServiceError(
                "Timed out while waiting for the playit service to finish starting. Try `playit status` to check its current state."
                    .to_string(),
            ));
        }
        AgentLifecycle::Stopping => {
            return Err(CliError::ServiceError(
                "The playit service is stopping. Try again in a few seconds.".to_string(),
            ));
        }
        AgentLifecycle::Error(error) => {
            return Err(CliError::ServiceError(format!(
                "The playit service reported an error and cannot continue: {}",
                error.message
            )));
        }
    }

    run_attach_command_with_context(
        target,
        attach_mode,
        AttachErrorContext::AutoCommand {
            start_attempt_failed,
        },
    )
    .await
}

async fn wait_for_auto_lifecycle(client: &mut IpcClient) -> Result<AgentLifecycle, CliError> {
    for _ in 0..50 {
        let lifecycle = client.lifecycle().await.map_err(|error| {
            CliError::IpcError(format!("Failed to read playitd lifecycle: {error}"))
        })?;

        if !matches!(lifecycle, AgentLifecycle::Starting) {
            return Ok(lifecycle);
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Ok(AgentLifecycle::Starting)
}

async fn reset_service_secret_for_setup(target: &CliTarget) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let response = client
        .reset_secret()
        .await
        .map_err(|error| CliError::IpcError(format!("Failed to reset secret: {error}")))?;

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

        let lifecycle = client.lifecycle().await.map_err(|error| {
            CliError::IpcError(format!("Failed to read playitd lifecycle: {error}"))
        })?;

        match lifecycle {
            AgentLifecycle::WaitingForSecret => return Ok(()),
            AgentLifecycle::HasInvalidSecret(_)
            | AgentLifecycle::DisabledOverLimit(_)
            | AgentLifecycle::Running(_)
            | AgentLifecycle::Starting => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            AgentLifecycle::Stopping => {
                return Err(CliError::ServiceError(
                    "The playit service is stopping and did not become ready for setup after the reset."
                        .to_string(),
                ));
            }
            AgentLifecycle::Error(error) => {
                return Err(CliError::ServiceError(format!(
                    "The playit service reported an error after the reset: {}",
                    error.message
                )));
            }
        }
    }

    Err(CliError::ServiceError(
        "Timed out while waiting for the playit service to become ready for setup after the reset."
            .to_string(),
    ))
}

async fn run_attach_command_with_context(
    target: &CliTarget,
    mode: AttachMode,
    error_context: AttachErrorContext,
) -> Result<(), CliError> {
    let mut client = connect_target(target)
        .await
        .map_err(|_| initial_attach_error(target, &error_context))?;

    let subscribe = client
        .subscribe()
        .await
        .map_err(|_| initial_attach_error(target, &error_context))?;

    match mode {
        AttachMode::Interactive => run_attach_tui_session(client, target, subscribe).await,
        AttachMode::Stdout => run_attach_stdout_session(client, target).await,
    }
}

async fn run_attach_tui_session(
    mut client: IpcClient,
    target: &CliTarget,
    subscribe: SubscribeResponse,
) -> Result<(), CliError> {
    let mut tui = TuiApp::new();
    tui.apply_status(subscribe.snapshot.status);
    tui.apply_lifecycle(subscribe.snapshot.lifecycle);
    tui.set_stats(ConnectionStats::from(subscribe.snapshot.stats));

    let _close_guard = crate::signal_handle::get_signal_handle().close_guard();

    loop {
        tokio::select! {
            update_result = client.recv_update() => {
                match update_result {
                    Ok(update) => apply_tui_update(&mut tui, update),
                    Err(error) => {
                        tui.shutdown()?;
                        println!("{}", attach_lost_message(target, &error.to_string()));
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                match tui.tick() {
                    Ok(true) => {}
                    Ok(false) => {
                        tui.shutdown()?;
                        print_detach_message();
                        break;
                    }
                    Err(error) => {
                        tui.shutdown()?;
                        return Err(error);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn run_attach_stdout_session(
    mut client: IpcClient,
    target: &CliTarget,
) -> Result<(), CliError> {
    loop {
        tokio::select! {
            update_result = client.recv_update() => {
                match update_result {
                    Ok(update) => apply_stdout_update(update),
                    Err(error) => {
                        eprintln!("{}", attach_lost_message(target, &error.to_string()));
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!();
                print_detach_message();
                break;
            }
        }
    }

    Ok(())
}

fn apply_tui_update(tui: &mut TuiApp, update: ServiceUpdate) {
    match update {
        ServiceUpdate::Lifecycle(state) => tui.apply_lifecycle(state),
        ServiceUpdate::Status(status) => tui.apply_status(status),
        ServiceUpdate::Stats(stats) => tui.set_stats(stats.into()),
        ServiceUpdate::Log(entry) => tui.push_service_log(entry),
    }
}

fn apply_stdout_update(update: ServiceUpdate) {
    if let ServiceUpdate::Log(entry) = update {
        println!(
            "{} {:>5} {}: {}",
            format_timestamp_millis(entry.timestamp),
            format_log_level(&entry.level),
            entry.target,
            entry.message
        );
    }
}

fn print_detach_message() {
    println!("Detached from service. Service continues running in background.");
    println!("Use 'playit stop' to stop the service.");
}

pub async fn run_start_command(
    console: &mut ConsoleUi,
    target: &CliTarget,
    service_manager: ServiceManagerMode,
) -> Result<(), CliError> {
    if let CliTarget::ExplicitSocket(path) = target {
        return Err(CliError::ServiceError(format!(
            "`playit start` only manages the installed background service. Remove `--socket-path {path}` or start that daemon manually."
        )));
    }

    #[cfg(target_os = "linux")]
    if matches!(service_manager, ServiceManagerMode::None) {
        return Err(CliError::ServiceError(
            "`playit start` can only start the installed service when run with --systemd or --openrc.\n\nIf you are managing playitd yourself, start it in the background and connect with --socket-path:\n  playitd --socket-path=./playit.sock --secret-path=./playit.toml\n  playit --socket-path=./playit.sock"
                .to_string(),
        ));
    }

    match ensure_installed_service_running_for_cli(Some(console), service_manager).await? {
        InstalledServiceStartState::AlreadyRunning => {
            println!("The playit service is already running.")
        }
        InstalledServiceStartState::Started => println!("The playit service started."),
    }
    println!("Run \"playit attach\" to view logs and tunnel status.");
    Ok(())
}

pub async fn run_stop_command(
    target: &CliTarget,
    service_manager: ServiceManagerMode,
) -> Result<(), CliError> {
    match target {
        CliTarget::InstalledService => {
            let mut direct_stop_fallback = true;

            match connect_target(target).await {
                Ok(mut client) => match client.stop().await {
                    Ok(response) if response.accepted => {
                        direct_stop_fallback = false;
                        println!("Asked the playit service to stop.");
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
                            "Could not reach the playit service over IPC. Trying the system service manager instead."
                        );
                    }
                },
                Err(error) => {
                    tracing::warn!("Failed to connect to installed service over IPC: {error}");
                    eprintln!(
                        "Could not reach the playit service over IPC. Trying the system service manager instead."
                    );
                }
            }

            if direct_stop_fallback {
                if matches!(
                    stop_installed_service_for_cli(service_manager)?,
                    InstalledServiceStopState::AlreadyStopped
                ) {
                    return Ok(());
                }
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
            if !IpcClient::is_running(get_default_socket_path()).await {
                println!("The playit service stopped.");
            } else {
                println!("The playit service may still be running. Run `playit status` to check.");
            }

            Ok(())
        }
        CliTarget::ExplicitSocket(path) => {
            let mut client = connect_target(target).await?;
            let response = client.stop().await.map_err(|error| {
                CliError::IpcError(format!("Failed to stop daemon at {path}: {error}"))
            })?;

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
                println!(
                    "The playit daemon may still be running. Check the daemon process for socket {path}."
                );
            }

            Ok(())
        }
    }
}

pub async fn run_status_command(target: &CliTarget) -> Result<(), CliError> {
    if !IpcClient::is_running(target.socket_path()).await {
        match target {
            CliTarget::InstalledService => println!("The playit service is not running."),
            CliTarget::ExplicitSocket(path) => {
                println!("The playit daemon is not reachable at socket {path}.")
            }
        }
        return Ok(());
    }

    let mut client = connect_target(target).await?;

    match client.status().await {
        Ok(status) => {
            match target {
                CliTarget::InstalledService => println!("playit service status:"),
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
            println!("  IPC version: {}", status.protocol.ipc_version);
            if !status.protocol.capabilities.is_empty() {
                println!("  Capabilities: {:?}", status.protocol.capabilities);
            }
            if matches!(status.phase, ServicePhase::DisabledOverLimit) {
                println!("  Message:");
                for line in agent_over_limit_title().lines() {
                    println!("    {line}");
                }
                for line in agent_over_limit_guidance().lines() {
                    println!("    {line}");
                }
            }
            if let Some(error) = status.last_error {
                println!("  Last error: {}", error.message);
            }
        }
        Err(_) => return Err(ipc_connection_error()),
    }

    Ok(())
}

pub async fn ensure_service_waiting_for_secret(
    console: &mut ConsoleUi,
    target: &CliTarget,
    service_manager: ServiceManagerMode,
) -> Result<(), CliError> {
    if matches!(target, CliTarget::InstalledService) {
        ensure_installed_service_running_for_cli(Some(console), service_manager).await?;
    }

    let mut client = connect_target(target).await?;
    let lifecycle = client.lifecycle().await.map_err(|error| {
        CliError::IpcError(format!("Failed to read playitd lifecycle: {error}"))
    })?;

    match lifecycle {
        AgentLifecycle::WaitingForSecret => Ok(()),
        AgentLifecycle::HasInvalidSecret(error) => Err(CliError::ServiceError(format!(
            "Setup cannot continue because the playit service has an invalid secret: {}. Run `playit reset`, then run `playit setup` again.",
            error.message
        ))),
        AgentLifecycle::DisabledOverLimit(_) => Err(CliError::ServiceError(format!(
            "{}\n{}",
            "Setup is unavailable because this account is over the agent limit.",
            agent_over_limit_guidance()
        ))),
        AgentLifecycle::Starting => Err(CliError::ServiceError(
            "The playit service is still starting. Try setup again in a few seconds.".to_string(),
        )),
        AgentLifecycle::Running(_) => Err(CliError::ServiceError(
            "The playit service already has a configured secret. Run `playit reset` before claiming a new agent."
                .to_string(),
        )),
        AgentLifecycle::Stopping => Err(CliError::ServiceError(
            "The playit service is stopping. Try setup again after it stops.".to_string(),
        )),
        AgentLifecycle::Error(error) => Err(CliError::ServiceError(format!(
            "The playit service reported an error and is not ready for setup: {}",
            error.message
        ))),
    }
}

pub async fn provision_service_secret(
    console: &mut ConsoleUi,
    target: &CliTarget,
    secret: &str,
    service_manager: ServiceManagerMode,
) -> Result<(), CliError> {
    if matches!(target, CliTarget::InstalledService) {
        ensure_installed_service_running_for_cli(Some(console), service_manager).await?;
    }

    let mut client = connect_target(target).await?;
    let response = client
        .set_secret(secret)
        .await
        .map_err(|error| CliError::IpcError(format!("Failed to provision secret: {error}")))?;

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
        .map_err(|error| CliError::IpcError(format!("Failed to reset secret: {error}")))?;

    if !reset_response.accepted {
        return Err(CliError::IpcError(reset_response.message.unwrap_or_else(
            || "playitd rejected the reset request".to_string(),
        )));
    }

    let stop_response = client.stop().await.map_err(|error| {
        CliError::IpcError(format!(
            "Secret was reset, but failed to stop playitd: {error}"
        ))
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
        .map_err(|error| CliError::IpcError(format!("Failed to read secret path: {error}")))?;

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
    let response = client.get_account_login_url().await.map_err(|error| {
        CliError::IpcError(format!("Failed to create account login URL: {error}"))
    })?;

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
        "Could not connect to the playit service. Start it with `playit start`, then try again."
            .to_string(),
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

pub(crate) fn auto_attach_error(
    target: &CliTarget,
    start_attempt_failed: Option<&str>,
) -> CliError {
    match target {
        CliTarget::InstalledService => match start_attempt_failed {
            #[cfg(target_os = "linux")]
            Some(error) if linux::is_linux_socket_access_message(error) => {
                CliError::IpcError(error.to_string())
            }
            Some(error) if error.starts_with("The playit service is running, but") => {
                CliError::IpcError(error.to_string())
            }
            Some(error) if error.starts_with("The playit service is not reachable") => {
                CliError::IpcError(error.to_string())
            }
            Some(error) => CliError::IpcError(format!(
                "Could not connect to the playit service. playit also tried to start it first, but startup failed: {error}"
            )),
            None => CliError::IpcError(
                "Could not connect to the playit service. playit tried to start it first, but it is still not reachable."
                    .to_string(),
            ),
        },
        CliTarget::ExplicitSocket(_) => ipc_connection_error(),
    }
}

fn attach_lost_message(target: &CliTarget, error: &str) -> String {
    match target {
        CliTarget::InstalledService => {
            format!(
                "Connection to the playit service was lost: {error}. Run \"playit attach\" to reconnect."
            )
        }
        CliTarget::ExplicitSocket(path) => format!(
            "Connection to the playit daemon was lost: {error}. Reattach with \"playit attach --socket-path {}\" once the daemon is reachable again.",
            path
        ),
    }
}

fn format_timestamp_millis(millis: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(millis as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
        .unwrap_or_else(|| format!("{millis}ms"))
}

fn format_service_phase(phase: &ServicePhase) -> &'static str {
    match phase {
        ServicePhase::WaitingForSecret => "waiting for secret",
        ServicePhase::HasInvalidSecret => "invalid secret",
        ServicePhase::DisabledOverLimit => "disabled over limit",
        ServicePhase::Starting => "starting",
        ServicePhase::Running => "running",
        ServicePhase::Stopping => "stopping",
        ServicePhase::Error => "error",
    }
}

fn agent_over_limit_guidance() -> String {
    format!(
        "Delete unused agents: {ACCOUNT_AGENTS_URL}\nIncrease your agent limit: {ACCOUNT_UPGRADE_URL}"
    )
}

fn agent_over_limit_title() -> &'static str {
    "The playit service cannot start because this account is over the agent limit."
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_start_command_requires_service_manager() {
        let mut console = ConsoleUi::new(crate::ui::UISettings { auto_answer: None });
        let error = run_start_command(
            &mut console,
            &CliTarget::InstalledService,
            ServiceManagerMode::None,
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "`playit start` can only start the installed service when run with --systemd or --openrc.\n\nIf you are managing playitd yourself, start it in the background and connect with --socket-path:\n  playitd --socket-path=./playit.sock --secret-path=./playit.toml\n  playit --socket-path=./playit.sock"
        );
    }
}
