use std::time::Duration;

use chrono::{DateTime, Utc};
use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playit_ipc::model::{
    AgentLifecycle, ClaimExchangeResponse, ClaimProgressResponse, ClaimSessionResponse,
    LogLevel as ServiceLogLevel, ServicePhase, ServiceUpdate, SubscribeResponse,
};
use playit_platform::service::{ServiceState, installed_service_state, stop_installed_service};
use playit_runtime::{
    GracefulStopAttempt, InstalledServiceStopOutcome, InstalledServiceStopPolicy,
    stop_installed_service_with_fallback,
};

#[cfg(target_os = "linux")]
use crate::linux;
use crate::problem::{lifecycle_message, render_ipc_error, render_problem, service_phase_label};
use crate::service::{
    InstalledServiceStartState, ServiceManagerMode, ensure_installed_service_running_for_cli,
    installed_service_manager,
};
use crate::ui::{ConnectionStats, ConsoleUi, TuiApp};
use crate::{CliError, run_setup_flow};

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
                        render_problem(&error)
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
            run_setup_flow(console, target, service_manager).await?;
        }
        AgentLifecycle::DisabledOverLimit(error) => {
            return Err(CliError::ServiceError(render_problem(&error)));
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
            return Err(CliError::ServiceError(render_problem(&error)));
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
    let response = client.reset_secret().await.map_err(|error| {
        CliError::IpcError(format!(
            "Failed to reset secret: {}",
            render_ipc_error(&error)
        ))
    })?;

    if !response.accepted {
        return Err(CliError::IpcError(response.message.unwrap_or_else(|| {
            "playitd rejected the reset request".to_string()
        })));
    }

    for _ in 0..50 {
        if !IpcClient::is_running(target.socket_path()).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Err(CliError::ServiceError(
        "The playit service accepted reset but did not stop before setup restarted.".to_owned(),
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
    match update {
        ServiceUpdate::Log(entry) => println!("{}", format_stdout_log(&entry)),
        ServiceUpdate::Lifecycle(lifecycle) => {
            if let Some(message) = lifecycle_message(&lifecycle) {
                println!("{message}");
            }
        }
        ServiceUpdate::Status(_) | ServiceUpdate::Stats(_) => {}
    }
}

fn format_stdout_log(entry: &playit_ipc::model::LogEntry) -> String {
    format!(
        "{} {:>5} {}: {}",
        format_timestamp_millis(entry.timestamp),
        format_log_level(&entry.level),
        entry.target,
        entry.message
    )
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
            let manager = installed_service_manager(service_manager)?;
            let report = stop_installed_service_with_fallback(
                InstalledServiceStopPolicy::default(),
                || async move {
                    tokio::task::spawn_blocking(move || installed_service_state(manager))
                        .await
                        .map_err(|error| format!("Failed to join service status task: {error}"))?
                        .map(|state| state == ServiceState::Running)
                        .map_err(|error| error.to_string())
                },
                || async {
                    match connect_target(target).await {
                        Ok(mut client) => match client.stop().await {
                            Ok(response) if response.accepted => GracefulStopAttempt::Accepted,
                            Ok(response) => GracefulStopAttempt::Rejected(
                                response
                                    .message
                                    .unwrap_or_else(|| "service rejected stop request".to_owned()),
                            ),
                            Err(error) => GracefulStopAttempt::Failed(error.to_string()),
                        },
                        Err(error) => GracefulStopAttempt::Failed(error.to_string()),
                    }
                },
                || async move {
                    tokio::task::spawn_blocking(move || stop_installed_service(manager))
                        .await
                        .map_err(|error| {
                            format!("Failed to join installed service stop task: {error}")
                        })?
                        .map_err(|error| error.to_string())
                },
            )
            .await;

            if let Some(error) = report.initial_status_error.as_deref() {
                tracing::warn!("Failed to query installed service before stop: {error}");
            }
            match report.graceful.as_ref() {
                Some(GracefulStopAttempt::Accepted) => {
                    println!("Asked the playit service to stop.")
                }
                Some(GracefulStopAttempt::Rejected(error)) => {
                    tracing::warn!("playitd rejected stop request: {error}");
                }
                Some(GracefulStopAttempt::Failed(error)) => {
                    tracing::warn!("Failed to send stop via IPC: {error}");
                    eprintln!(
                        "Could not reach the playit service over IPC. Trying the system service manager instead."
                    );
                }
                None => {}
            }
            if let Some(Err(error)) = report.fallback.as_ref() {
                tracing::warn!("Failed to stop installed service: {error}");
            }
            match report.outcome {
                InstalledServiceStopOutcome::AlreadyStopped => {
                    println!("The playit service is already stopped.");
                }
                InstalledServiceStopOutcome::Stopped => println!("The playit service stopped."),
                InstalledServiceStopOutcome::StillRunning => {
                    println!(
                        "The playit service may still be running. Run `playit status` to check."
                    );
                }
                InstalledServiceStopOutcome::StatusUnknown(error) => {
                    tracing::warn!("Failed to query installed service after stop: {error}");
                    println!(
                        "The playit service stop was requested. Run `playit status` to check."
                    );
                }
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
            println!("  Phase: {}", service_phase_label(&status.phase));
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
                for line in crate::problem::render_problem_code(
                    playit_ipc::model::ServiceErrorCode::AgentDisabledOverLimit,
                )
                .lines()
                {
                    println!("    {line}");
                }
            }
            if let Some(error) = status.last_error {
                println!("  Last error: {}", render_problem(&error));
            }
        }
        Err(_) => return Err(ipc_connection_error()),
    }

    Ok(())
}

pub async fn begin_service_claim(
    console: &mut ConsoleUi,
    target: &CliTarget,
    service_manager: ServiceManagerMode,
) -> Result<ClaimSessionResponse, CliError> {
    if matches!(target, CliTarget::InstalledService) {
        ensure_installed_service_running_for_cli(Some(console), service_manager).await?;
    }

    let mut client = connect_target(target).await?;
    client.begin_claim().await.map_err(|error| {
        CliError::IpcError(format!(
            "Failed to begin setup: {}",
            render_ipc_error(&error)
        ))
    })
}

pub async fn complete_service_claim(
    console: &mut ConsoleUi,
    target: &CliTarget,
    claim_code: &str,
) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let _close_guard = crate::signal_handle::get_signal_handle().close_guard();

    loop {
        let progress = match client.poll_claim(claim_code).await {
            Ok(progress) => progress,
            Err(error) => {
                console
                    .write_screen(format!(
                        "Setup is waiting for the service.\n\nError: {}",
                        render_ipc_error(&error)
                    ))
                    .await;
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        match progress {
            ClaimProgressResponse::WaitingForVisit => {}
            ClaimProgressResponse::WaitingForApproval => {
                console
                    .write_screen("Approve this program in your browser to continue setup.")
                    .await;
            }
            ClaimProgressResponse::Approved => {
                console
                    .write_screen("Program approved. Finishing setup...")
                    .await;
                break;
            }
            ClaimProgressResponse::Rejected => return Err(CliError::AgentClaimRejected),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    loop {
        match client.exchange_claim(claim_code).await.map_err(|error| {
            CliError::IpcError(format!(
                "Failed to finish setup: {}",
                render_ipc_error(&error)
            ))
        })? {
            ClaimExchangeResponse::Accepted => return Ok(()),
            ClaimExchangeResponse::Pending(status) => {
                console
                    .write_screen(format!("Waiting for setup to finish: {status}"))
                    .await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

pub async fn run_reset_command(target: &CliTarget) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let reset_response = client.reset_secret().await.map_err(|error| {
        CliError::IpcError(format!(
            "Failed to reset secret: {}",
            render_ipc_error(&error)
        ))
    })?;

    if !reset_response.accepted {
        return Err(CliError::IpcError(reset_response.message.unwrap_or_else(
            || "playitd rejected the reset request".to_string(),
        )));
    }

    let reset_message = reset_response
        .message
        .unwrap_or_else(|| "playitd reset the secret file".to_string());
    println!("{reset_message}");
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

    #[test]
    fn user_facing_output_contract_is_stable() {
        let phases = [
            ServicePhase::WaitingForSecret,
            ServicePhase::HasInvalidSecret,
            ServicePhase::DisabledOverLimit,
            ServicePhase::Starting,
            ServicePhase::Running,
            ServicePhase::Stopping,
            ServicePhase::Error,
        ];
        let mut lines: Vec<String> = phases
            .iter()
            .map(|phase| service_phase_label(phase).to_string())
            .collect();
        lines.push(crate::problem::over_limit_title().to_string());
        lines.extend(
            crate::problem::over_limit_guidance()
                .lines()
                .map(str::to_string),
        );
        lines.push(attach_lost_message(
            &CliTarget::InstalledService,
            "connection reset",
        ));
        lines.push(attach_lost_message(
            &CliTarget::ExplicitSocket("./fixture.sock".to_string()),
            "connection reset",
        ));
        lines.push(auto_attach_error(&CliTarget::InstalledService, None).to_string());
        lines.push(
            auto_attach_error(
                &CliTarget::InstalledService,
                Some("fixture startup failure"),
            )
            .to_string(),
        );
        lines.push(format_stdout_log(&playit_ipc::model::LogEntry {
            level: ServiceLogLevel::Warn,
            target: "playitd::fixture".to_string(),
            message: "fixture log".to_string(),
            timestamp: 1_700_000_000_123,
        }));

        assert_eq!(
            lines.join("\n"),
            include_str!("../fixtures/output_contract.txt").trim_end()
        );
    }
}
