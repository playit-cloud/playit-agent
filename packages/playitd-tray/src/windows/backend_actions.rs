use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playit_ipc::model::AgentLifecycle;
use playitd::manager::{
    InstalledServiceState, ensure_installed_service_running, installed_service_state,
    stop_installed_service,
};
use tokio::task;
use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

use super::protocol::{BackendRequest, BackendRequestKind, BackendResponse};
use super::util::debug_log;
use crate::presentation::ServiceView;

pub(super) async fn handle_request(request: BackendRequest) -> Option<BackendResponse> {
    debug_log(&format!("backend_actions: handling request {request:?}"));

    let (request_kind, error) = match request {
        BackendRequest::RefreshStatus => (BackendRequestKind::RefreshStatus, None),
        BackendRequest::StartService => (
            BackendRequestKind::StartService,
            start_service().await.err(),
        ),
        BackendRequest::StopService => {
            (BackendRequestKind::StopService, stop_service().await.err())
        }
        BackendRequest::ResetAgent => (BackendRequestKind::ResetAgent, reset_agent().await.err()),
        BackendRequest::Shutdown => return None,
    };

    let view = query_service_view().await;
    debug_log(&format!(
        "backend_actions: completed {request_kind:?} view={view:?} error={}",
        error.as_deref().unwrap_or("<none>")
    ));

    Some(BackendResponse::RequestCompleted {
        request: request_kind,
        view,
        error,
    })
}

/// Opens the playit window: setup when the agent needs it, otherwise live status.
pub(super) fn launch_playit() -> Result<(), String> {
    let cli_path = sibling_executable("playit.exe")?;
    debug_log(&format!("launch_playit: launching {}", cli_path.display()));
    Command::new(cli_path)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .map_err(|error| format!("Could not open playit.exe: {error}"))?;
    Ok(())
}

pub(super) fn open_dashboard() -> Result<(), String> {
    playitd::windows::open_url(playitd::windows::DASHBOARD_URL)
        .map_err(|error| format!("Could not open the playit dashboard: {error}"))
}

pub(super) fn open_logs_folder() -> Result<(), String> {
    let log_dir = playitd::windows_service_log_dir();
    playitd::windows::open_folder(&log_dir)
        .map_err(|error| format!("Could not open {}: {error}", log_dir.display()))
}

pub(super) fn response_error_title(request: BackendRequestKind) -> &'static str {
    match request {
        BackendRequestKind::RefreshStatus => "Failed to refresh playit status",
        BackendRequestKind::StartService => "Failed to start the playit service",
        BackendRequestKind::StopService => "Failed to stop the playit service",
        BackendRequestKind::ResetAgent => "Failed to reset the playit agent",
    }
}

async fn start_service() -> Result<(), String> {
    ensure_installed_service_running()
        .await
        .map_err(|error| error.to_string())
}

async fn stop_service() -> Result<(), String> {
    // Prefer the service manager; fall back to asking the daemon directly, which also
    // works if the user's service permissions were never repaired.
    let scm_result = task::spawn_blocking(stop_installed_service)
        .await
        .map_err(|error| format!("Failed to stop the service: {error}"))?;

    if let Err(scm_error) = scm_result {
        debug_log(&format!("stop_service: SCM stop failed: {scm_error}"));
        let mut client = IpcClient::connect()
            .await
            .map_err(|_| scm_error.to_string())?;
        client
            .stop()
            .await
            .map_err(|error| format!("Could not stop the playit service: {error}"))?;
    }

    for _ in 0..50 {
        if !IpcClient::is_running(get_default_socket_path()).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    Err("The playit service is still stopping. Check again in a few seconds.".to_string())
}

async fn reset_agent() -> Result<(), String> {
    let mut client = IpcClient::connect()
        .await
        .map_err(|error| format!("Could not connect to the playit service: {error}"))?;
    let response = client
        .reset_secret()
        .await
        .map_err(|error| format!("Could not reset the agent: {error}"))?;
    if !response.accepted {
        return Err(response
            .message
            .unwrap_or_else(|| "The playit service rejected the reset request.".to_string()));
    }

    // The daemon returns to setup on its own; wait for it so the window opens into setup.
    for _ in 0..25 {
        if matches!(
            query_lifecycle().await,
            Ok(AgentLifecycle::WaitingForSecret)
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    launch_playit()
}

pub(super) async fn query_service_view() -> ServiceView {
    let state = match task::spawn_blocking(installed_service_state).await {
        Ok(Ok(state)) => state,
        Ok(Err(error)) => return ServiceView::Unknown(error.to_string()),
        Err(error) => return ServiceView::Unknown(error.to_string()),
    };

    match state {
        InstalledServiceState::Running => match query_lifecycle().await {
            Ok(lifecycle) => ServiceView::Agent(lifecycle),
            // Running but not answering yet: the daemon is still binding its pipe.
            Err(_) => ServiceView::Starting,
        },
        InstalledServiceState::Starting => ServiceView::Starting,
        InstalledServiceState::Stopping => ServiceView::Stopping,
        InstalledServiceState::Stopped => ServiceView::Stopped,
        InstalledServiceState::NotInstalled => ServiceView::NotInstalled,
        InstalledServiceState::Unknown => ServiceView::Unknown("unknown service state".to_string()),
    }
}

async fn query_lifecycle() -> Result<AgentLifecycle, String> {
    let mut client = IpcClient::connect()
        .await
        .map_err(|error| error.to_string())?;
    client.lifecycle().await.map_err(|error| error.to_string())
}

fn sibling_executable(name: &str) -> Result<PathBuf, String> {
    std::env::current_exe()
        .map(|path| path.with_file_name(name))
        .map_err(|error| format!("Failed to resolve {name}: {error}"))
}
