use std::ffi::c_void;
use std::fs;
use std::mem::zeroed;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::ptr::{null, null_mut};

use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playit_ipc::model::AgentLifecycle;
use playitd::manager::{
    INSTALLED_SERVICE_LABEL, ensure_installed_service_running, stop_installed_service,
};
use tokio::task;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT,
    SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
};
use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_CommonStartup, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};

use super::protocol::{BackendRequest, BackendRequestKind, BackendResponse, ServiceStateSnapshot};
use super::util::{debug_log, wide};

const TRAY_SHORTCUT_NAME: &str = "Playit Tray.lnk";

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
        .map_err(|error| format!("Failed to launch playit.exe: {error}"))?;
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
        .map_err(|error| format!("Failed to launch playit.exe attach: {error}"))?;
    Ok(())
}

pub(super) fn response_error_title(request: BackendRequestKind) -> &'static str {
    match request {
        BackendRequestKind::RefreshStatus => "Failed to refresh playit tray",
        BackendRequestKind::StartService => "Failed to start playitd service",
        BackendRequestKind::StopService => "Failed to stop playitd service",
        BackendRequestKind::ResetAgent => "Failed to reset playit agent",
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
        .map_err(|error| format!("Failed waiting for playitd service startup: {error}"));

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
        Err("playitd service may still be running".to_string())
    } else {
        debug_log("stop_service: final running check says service stopped");
        Ok(())
    }
}

async fn reset_agent_async() -> Result<(), String> {
    debug_log("reset_agent: begin");
    if !query_service_running_async().await {
        debug_log("reset_agent: service is not running");
        return Err("playitd is not running, so Reset Agent is unavailable".to_string());
    }

    let lifecycle = query_service_lifecycle_async().await;
    debug_log(&format!(
        "reset_agent: lifecycle query result = {lifecycle:?}"
    ));
    if matches!(lifecycle, Ok(AgentLifecycle::WaitingForSecret)) {
        debug_log("reset_agent: lifecycle already WaitingForSecret");
        return Err(
            "playitd is already waiting for setup, so Reset Agent is unavailable".to_string(),
        );
    }

    debug_log("reset_agent: connecting to playitd over IPC");
    let mut client = IpcClient::connect().await.map_err(|error| {
        debug_log(&format!("reset_agent: failed to connect over IPC: {error}"));
        format!("Failed to connect to playitd over IPC: {error}")
    })?;

    debug_log("reset_agent: sending reset_secret request");
    let reset_response = client.reset_secret().await.map_err(|error| {
        debug_log(&format!(
            "reset_agent: reset_secret IPC call failed: {error}"
        ));
        format!("Failed to reset agent over IPC: {error}")
    })?;

    debug_log(&format!(
        "reset_agent: reset_secret response accepted={} message={}",
        reset_response.accepted,
        reset_response.message.as_deref().unwrap_or("<none>")
    ));

    if !reset_response.accepted {
        return Err(reset_response
            .message
            .unwrap_or_else(|| "playitd rejected the reset request".to_string()));
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
        format!("Failed to connect to playitd over IPC: {error}")
    })?;

    let lifecycle = client.lifecycle().await.map_err(|error| {
        debug_log(&format!(
            "lifecycle: failed to read lifecycle over IPC: {error}"
        ));
        format!("Failed to read playitd lifecycle over IPC: {error}")
    })?;

    debug_log(&format!("lifecycle: received {lifecycle:?}"));
    Ok(lifecycle)
}

pub(super) fn remove_startup_shortcut() -> Result<(), String> {
    let shortcut_path = startup_shortcut_path()?;

    if !shortcut_path.exists() {
        debug_log("remove_startup_shortcut: shortcut does not exist");
        return Ok(());
    }

    debug_log(&format!(
        "remove_startup_shortcut: deleting {}",
        shortcut_path.display()
    ));
    fs::remove_file(&shortcut_path).map_err(|error| {
        format!(
            "Failed to delete startup shortcut at {}: {error}",
            shortcut_path.display()
        )
    })
}

pub(super) fn query_service_running_sync() -> bool {
    unsafe {
        let manager = OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            debug_log("service_query: failed to open SCM");
            return false;
        }

        let service_name = wide(INSTALLED_SERVICE_LABEL);
        let service = OpenServiceW(manager, service_name.as_ptr(), SERVICE_QUERY_STATUS);
        if service.is_null() {
            debug_log("service_query: failed to open playitd service");
            let _ = CloseServiceHandle(manager);
            return false;
        }

        let mut status = zeroed::<SERVICE_STATUS_PROCESS>();
        let mut bytes_needed = 0;
        let running = QueryServiceStatusEx(
            service,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut bytes_needed,
        ) != 0
            && status.dwCurrentState == SERVICE_RUNNING;

        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(manager);
        debug_log(&format!("service_query: playitd running={running}"));
        running
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

fn startup_shortcut_path() -> Result<PathBuf, String> {
    unsafe {
        let mut wide_path = null_mut();
        let result = SHGetKnownFolderPath(
            &FOLDERID_CommonStartup,
            KF_FLAG_DEFAULT as u32,
            null_mut(),
            &mut wide_path,
        );

        if result < 0 {
            return Err(format!(
                "Failed to resolve the common Startup folder (HRESULT {result:#x})"
            ));
        }

        if wide_path.is_null() {
            return Err("Common Startup folder path was empty".to_string());
        }

        let mut len = 0usize;
        while *wide_path.add(len) != 0 {
            len += 1;
        }

        let path = std::ffi::OsString::from_wide(std::slice::from_raw_parts(wide_path, len));
        CoTaskMemFree(wide_path.cast::<c_void>());

        Ok(PathBuf::from(path).join(TRAY_SHORTCUT_NAME))
    }
}
