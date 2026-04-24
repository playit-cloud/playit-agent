use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playit_ipc::model::AgentLifecycle;
use playitd::manager::{
    ensure_installed_service_running, installed_service_is_running, stop_installed_service,
};
use tokio::task;
use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

use super::protocol::{BackendRequest, BackendRequestKind, BackendResponse, ServiceStateSnapshot};
use super::startup_shortcut;
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

    let result = ensure_installed_service_running()
        .await
        .map_err(|error| format!("The background service did not start: {error}"));

    if result.is_ok() {
        debug_log("start_service: service started");
    }

    result
}

async fn stop_service_async() -> Result<(), String> {
    debug_log("stop_service: begin");
    if !query_service_running_async().await {
        debug_log("stop_service: service is already stopped");
        return Ok(());
    }

    debug_log("stop_service: attempting graceful IPC stop");

    match IpcClient::connect().await {
        Ok(mut client) => match client.stop().await {
            Ok(response) if response.accepted => {
                debug_log("stop_service: IPC stop request accepted");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Ok(response) => {
                debug_log(&format!(
                    "stop_service: IPC stop request rejected: {}",
                    response
                        .message
                        .unwrap_or_else(|| "service rejected stop request".to_string())
                ));
            }
            Err(error) => {
                debug_log(&format!("stop_service: IPC stop request failed: {error}"));
            }
        },
        Err(error) => {
            debug_log(&format!(
                "stop_service: failed to connect to playitd over IPC: {error}"
            ));
        }
    }

    match stop_installed_service_async().await {
        Ok(()) => debug_log("stop_service: direct SCM stop request completed"),
        Err(error) => {
            debug_log(&format!(
                "stop_service: direct SCM stop request failed: {error}"
            ));
        }
    }

    debug_log("stop_service: waiting before final running check");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    if IpcClient::is_running(get_default_socket_path()).await {
        debug_log("stop_service: final running check says playitd is still reachable");
        Err("The playit service may still be running. Try again in a few seconds or stop it from Windows Services.".to_string())
    } else {
        debug_log("stop_service: final running check says service stopped");
        Ok(())
    }
}

async fn reset_agent_async() -> Result<(), String> {
    debug_log("reset_agent: begin");
    if !query_service_running_async().await {
        debug_log("reset_agent: service is not running");
        return Err(
            "Reset Agent Setup is unavailable because the playit service is not running."
                .to_string(),
        );
    }

    let lifecycle = query_service_lifecycle_async().await;
    debug_log(&format!(
        "reset_agent: lifecycle query result = {lifecycle:?}"
    ));
    if matches!(lifecycle, Ok(AgentLifecycle::WaitingForSecret)) {
        debug_log("reset_agent: lifecycle already WaitingForSecret");
        return Err(
            "The playit service is already waiting for setup. Open playit to finish setup."
                .to_string(),
        );
    }

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
        format!("Could not reset the agent setup: {error}")
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

    debug_log("reset_agent: secret reset accepted, stopping service");
    stop_service_async().await?;
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
        reset_agent_enabled: query_reset_agent_enabled_async(service_running).await,
    }
}

async fn query_reset_agent_enabled_async(service_running: bool) -> bool {
    if !service_running {
        debug_log("snapshot: reset agent disabled because service is not running");
        return false;
    }

    match query_service_lifecycle_async().await {
        Ok(AgentLifecycle::WaitingForSecret) => {
            debug_log("snapshot: reset agent disabled because lifecycle is WaitingForSecret");
            false
        }
        Ok(AgentLifecycle::Stopping) => {
            debug_log("snapshot: reset agent disabled because lifecycle is Stopping");
            false
        }
        Ok(other) => {
            debug_log(&format!(
                "snapshot: reset agent enabled because lifecycle is {other:?}"
            ));
            true
        }
        Err(error) => {
            debug_log(&format!(
                "snapshot: lifecycle query failed while checking reset availability: {error}"
            ));
            true
        }
    }
}

async fn query_service_lifecycle_async() -> Result<AgentLifecycle, String> {
    debug_log("lifecycle: connecting to playitd over IPC");
    let mut client = IpcClient::connect().await.map_err(|error| {
        debug_log(&format!("lifecycle: failed to connect over IPC: {error}"));
        format!("Could not connect to the playit service: {error}")
    })?;

    let lifecycle = client.lifecycle().await.map_err(|error| {
        debug_log(&format!(
            "lifecycle: failed to read lifecycle over IPC: {error}"
        ));
        format!("Could not read the playit service status: {error}")
    })?;

    debug_log(&format!("lifecycle: received {lifecycle:?}"));
    Ok(lifecycle)
}

pub(super) fn remove_startup_shortcut() -> Result<(), String> {
    startup_shortcut::remove_startup_shortcut()
}

pub(super) fn ensure_startup_shortcut() -> Result<(), String> {
    startup_shortcut::ensure_startup_shortcut()
}

pub(super) fn startup_shortcut_exists() -> Result<bool, String> {
    startup_shortcut::startup_shortcut_exists()
}

pub(super) fn write_installed_user_sid() -> Result<(), String> {
    let path = playitd::windows::write_current_user_sid()
        .map_err(|error| format!("Failed to write installed user SID: {error}"))?;
    debug_log(&format!(
        "write_installed_user_sid: wrote {}",
        path.display()
    ));
    Ok(())
}

pub(super) fn query_service_running_sync() -> bool {
    match installed_service_is_running() {
        Ok(running) => {
            debug_log(&format!("service_query: playitd running={running}"));
            running
        }
        Err(error) => {
            debug_log(&format!("service_query: failed to query playitd: {error}"));
            false
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

async fn stop_installed_service_async() -> Result<(), String> {
    debug_log("stop_service: invoking direct SCM stop fallback");
    task::spawn_blocking(stop_installed_service)
        .await
        .map_err(|error| format!("Failed to join installed service stop task: {error}"))?
        .map_err(|error| error.to_string())
}

fn playit_cli_path() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map(|path| path.with_file_name("playit.exe"))
        .map_err(|error| format!("Failed to resolve playit.exe path: {error}"))
}
