use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use playit_ipc::ipc::{IpcClient, IpcError, get_default_socket_path};
use playit_ipc::model::ServiceError;
use playit_platform::service::{
    ReachabilityPolicy, ServiceManagerKind, ServiceState, installed_service_state,
    start_and_wait_until_reachable, stop_installed_service,
};
use playit_runtime::{
    GracefulStopAttempt, InstalledServiceStopOutcome, InstalledServiceStopPolicy,
    stop_installed_service_with_fallback,
};
use tokio::task;
use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

use super::protocol::{BackendRequest, BackendRequestKind, BackendResponse, ServiceStateSnapshot};
use super::util::debug_log;

pub(super) async fn handle_request(request: BackendRequest) -> Option<BackendResponse> {
    debug_log(&format!("backend_actions: handling request {request:?}"));

    let (request_kind, error) = match request {
        BackendRequest::RefreshStatus => (BackendRequestKind::RefreshStatus, None),
        BackendRequest::StartService => (
            BackendRequestKind::StartService,
            start_service_async().await.err(),
        ),
        BackendRequest::StopService => (
            BackendRequestKind::StopService,
            stop_service_async().await.err(),
        ),
        BackendRequest::ResetAgent => (
            BackendRequestKind::ResetAgent,
            reset_agent_async().await.err(),
        ),
        BackendRequest::Shutdown => return None,
    };

    let snapshot = query_service_state_snapshot_async().await;
    debug_log(&format!(
        "backend_actions: completed request {request_kind:?} with snapshot service_running={} reset_agent_enabled={} error={}",
        snapshot.service_running,
        snapshot.reset_agent_enabled,
        error.as_deref().unwrap_or("<none>")
    ));

    Some(BackendResponse::RequestCompleted {
        request: request_kind,
        snapshot,
        error,
    })
}

pub(super) fn launch_playit() -> Result<(), String> {
    let cli_path = playit_cli_path()?;
    debug_log(&format!("launch_playit: launching {}", cli_path.display()));
    Command::new(cli_path)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .map_err(|error| format!("Could not open playit.exe: {error}"))?;
    Ok(())
}

pub(super) fn launch_status_window() -> Result<(), String> {
    let cli_path = playit_cli_path()?;
    debug_log(&format!(
        "launch_status_window: launching {} attach",
        cli_path.display()
    ));
    Command::new(cli_path)
        .creation_flags(CREATE_NEW_CONSOLE)
        .arg("attach")
        .spawn()
        .map_err(|error| format!("Could not open the playit status window: {error}"))?;
    Ok(())
}

pub(super) fn response_error_title(request: BackendRequestKind) -> &'static str {
    match request {
        BackendRequestKind::RefreshStatus => "Failed to refresh playit tray",
        BackendRequestKind::StartService => "Failed to start playit service",
        BackendRequestKind::StopService => "Failed to stop playit service",
        BackendRequestKind::ResetAgent => "Failed to reset playit agent setup",
    }
}

async fn start_service_async() -> Result<(), String> {
    debug_log("start_service: begin");
    if query_service_running_async().await {
        debug_log("start_service: service is already running");
        return Ok(());
    }

    debug_log("start_service: ensuring installed service is running");

    let result = start_and_wait_until_reachable(
        ServiceManagerKind::WindowsScm,
        ReachabilityPolicy::default(),
        || IpcClient::is_running(get_default_socket_path()),
    )
    .await
    .map_err(|error| format!("The background service did not start: {error}"));

    if result.is_ok() {
        debug_log("start_service: service started");
    }

    result
}

async fn stop_service_async() -> Result<(), String> {
    debug_log("stop_service: begin");
    let report = stop_installed_service_with_fallback(
        InstalledServiceStopPolicy::default(),
        query_service_running_result_async,
        || async {
            match IpcClient::connect().await {
                Ok(mut client) => match client.stop().await {
                    Ok(response) if response.accepted => GracefulStopAttempt::Accepted,
                    Ok(response) => GracefulStopAttempt::Rejected(
                        response
                            .message
                            .unwrap_or_else(|| "service rejected stop request".to_owned()),
                    ),
                    Err(error) => GracefulStopAttempt::Failed(render_ipc_error(&error)),
                },
                Err(error) => GracefulStopAttempt::Failed(error.to_string()),
            }
        },
        stop_installed_service_async,
    )
    .await;

    if let Some(error) = report.initial_status_error.as_deref() {
        debug_log(&format!(
            "stop_service: initial service status failed: {error}"
        ));
    }
    match report.graceful.as_ref() {
        Some(GracefulStopAttempt::Accepted) => debug_log("stop_service: IPC stop request accepted"),
        Some(GracefulStopAttempt::Rejected(error)) => {
            debug_log(&format!("stop_service: IPC stop request rejected: {error}"))
        }
        Some(GracefulStopAttempt::Failed(error)) => {
            debug_log(&format!("stop_service: IPC stop request failed: {error}"))
        }
        None => {}
    }
    match report.fallback.as_ref() {
        Some(Ok(())) => debug_log("stop_service: direct SCM stop request completed"),
        Some(Err(error)) => debug_log(&format!(
            "stop_service: direct SCM stop request failed: {error}"
        )),
        None => {}
    }
    match report.outcome {
        InstalledServiceStopOutcome::AlreadyStopped => {
            debug_log("stop_service: service is already stopped");
            Ok(())
        }
        InstalledServiceStopOutcome::Stopped => {
            debug_log("stop_service: final service status says stopped");
            Ok(())
        }
        InstalledServiceStopOutcome::StillRunning => Err(
            "The playit service may still be running. Try again in a few seconds or stop it from Windows Services."
                .to_owned(),
        ),
        InstalledServiceStopOutcome::StatusUnknown(error) => Err(format!(
            "The playit service stop was requested, but its status could not be checked: {error}"
        )),
    }
}

async fn reset_agent_async() -> Result<(), String> {
    debug_log("reset_agent: begin");
    debug_log("reset_agent: connecting to playitd over IPC");
    let mut client = IpcClient::connect().await.map_err(|error| {
        debug_log(&format!("reset_agent: failed to connect over IPC: {error}"));
        format!("Could not connect to the playit service: {error}")
    })?;

    debug_log("reset_agent: sending reset_secret request");
    let reset_response = client.reset_secret().await.map_err(|error| {
        debug_log(&format!(
            "reset_agent: reset_secret IPC call failed: {error}"
        ));
        format!(
            "Could not reset the agent setup: {}",
            render_ipc_error(&error)
        )
    })?;

    debug_log(&format!(
        "reset_agent: reset_secret response accepted={} message={}",
        reset_response.accepted,
        reset_response.message.as_deref().unwrap_or("<none>")
    ));

    if !reset_response.accepted {
        return Err(reset_response
            .message
            .unwrap_or_else(|| "The playit service rejected the reset request.".to_string()));
    }

    debug_log("reset_agent: secret reset accepted; supervisor is stopping");
    for _ in 0..20 {
        if !IpcClient::is_running(get_default_socket_path()).await {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    debug_log("reset_agent: launching playit after stop");
    launch_playit()
}

async fn query_service_state_snapshot_async() -> ServiceStateSnapshot {
    let service_running = query_service_running_async().await;
    debug_log(&format!(
        "snapshot: service_running={} before reset-agent availability check",
        service_running
    ));

    ServiceStateSnapshot {
        service_running,
        reset_agent_enabled: service_running,
    }
}

fn render_ipc_error(error: &IpcError) -> String {
    match error {
        IpcError::Service(problem) => render_service_problem(problem),
        _ => error.to_string(),
    }
}

fn render_service_problem(problem: &ServiceError) -> String {
    match problem.meaning().code.as_str() {
        "invalid_secret" => "The configured secret is invalid.".to_owned(),
        "secret_pinned" => "Reset is unavailable while playitd uses an inline --secret.".to_owned(),
        "secret_write_failed" => {
            format!(
                "The playit service could not update its secret: {}",
                problem.message
            )
        }
        "command_not_allowed" => "Reset is not available in the current service state.".to_owned(),
        _ => problem.message.clone(),
    }
}

pub(super) fn remove_startup_shortcut() -> Result<(), String> {
    playit_platform::windows::remove_tray_startup_shortcut()
}

pub(super) fn ensure_startup_shortcut() -> Result<(), String> {
    let tray_path = std::env::current_exe()
        .map_err(|error| format!("Failed to resolve playitd-tray.exe path: {error}"))?;
    playit_platform::windows::ensure_tray_startup_shortcut(&tray_path).map(|_| ())
}

pub(super) fn startup_shortcut_exists() -> Result<bool, String> {
    playit_platform::windows::tray_startup_shortcut_exists()
}

pub(super) fn query_service_running_sync() -> bool {
    match query_service_running_result_sync() {
        Ok(running) => running,
        Err(_) => false,
    }
}

fn query_service_running_result_sync() -> Result<bool, String> {
    match installed_service_state(ServiceManagerKind::WindowsScm) {
        Ok(state) => {
            let running = state == ServiceState::Running;
            debug_log(&format!("service_query: playitd running={running}"));
            Ok(running)
        }
        Err(error) => {
            debug_log(&format!("service_query: failed to query playitd: {error}"));
            Err(error.to_string())
        }
    }
}

async fn query_service_running_async() -> bool {
    match task::spawn_blocking(query_service_running_sync).await {
        Ok(service_running) => {
            debug_log(&format!(
                "service_query_async: playitd running={service_running}"
            ));
            service_running
        }
        Err(error) => {
            debug_log(&format!(
                "service_query_async: failed to join query task: {error}"
            ));
            false
        }
    }
}

async fn query_service_running_result_async() -> Result<bool, String> {
    task::spawn_blocking(query_service_running_result_sync)
        .await
        .map_err(|error| format!("Failed to join service status task: {error}"))?
}

async fn stop_installed_service_async() -> Result<(), String> {
    debug_log("stop_service: invoking direct SCM stop fallback");
    task::spawn_blocking(|| stop_installed_service(ServiceManagerKind::WindowsScm))
        .await
        .map_err(|error| format!("Failed to join installed service stop task: {error}"))?
        .map_err(|error| error.to_string())
}

fn playit_cli_path() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map(|path| path.with_file_name("playit.exe"))
        .map_err(|error| format!("Failed to resolve playit.exe path: {error}"))
}
