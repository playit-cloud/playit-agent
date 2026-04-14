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

use super::state::{BackgroundAction, BackgroundActionResult, ServiceStateSnapshot};
use super::util::{debug_log, wide};

const TRAY_SHORTCUT_NAME: &str = "Playit Tray.lnk";

pub(super) fn launch_playit() -> Result<(), String> {
    let cli_path = playit_cli_path()?;
    Command::new(cli_path)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .map_err(|error| format!("Failed to launch playit.exe: {error}"))?;
    Ok(())
}

pub(super) fn launch_status_window() -> Result<(), String> {
    let cli_path = playit_cli_path()?;
    Command::new(cli_path)
        .creation_flags(CREATE_NEW_CONSOLE)
        .arg("attach")
        .spawn()
        .map_err(|error| format!("Failed to launch playit.exe attach: {error}"))?;
    Ok(())
}

pub(super) async fn start_service_async() -> Result<(), String> {
    if query_service_running_async().await {
        debug_log("start requested but service is already running");
        return Ok(());
    }

    debug_log("starting service");

    let result = ensure_installed_service_running()
        .await
        .map_err(|error| format!("Failed waiting for playitd service startup: {error}"));

    if result.is_ok() {
        debug_log("service started");
    }

    result
}

pub(super) async fn stop_service_async() -> Result<(), String> {
    if !query_service_running_async().await {
        debug_log("stop requested but service is already stopped");
        return Ok(());
    }

    debug_log("stopping service");

    match IpcClient::connect().await {
        Ok(mut client) => match client.stop().await {
            Ok(response) if response.accepted => {
                debug_log("service stop requested over IPC");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Ok(response) => {
                debug_log(&format!(
                    "service stop request over IPC was rejected: {}",
                    response
                        .message
                        .unwrap_or_else(|| "service rejected stop request".to_string())
                ));
            }
            Err(error) => {
                debug_log(&format!("failed to send stop over IPC: {error}"));
            }
        },
        Err(error) => {
            debug_log(&format!("failed to connect to playitd over IPC: {error}"));
        }
    }

    if let Err(error) = stop_installed_service_async().await {
        debug_log(&format!(
            "failed to stop installed service directly: {error}"
        ));
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    if IpcClient::is_running(get_default_socket_path()).await {
        Err("playitd service may still be running".to_string())
    } else {
        debug_log("service stopped");
        Ok(())
    }
}

pub(super) async fn run_background_action_async(
    action: BackgroundAction,
) -> BackgroundActionResult {
    let error = match action {
        BackgroundAction::RefreshStatus => None,
        BackgroundAction::StartService => start_service_async().await.err(),
        BackgroundAction::StopService => stop_service_async().await.err(),
        BackgroundAction::ResetAgent => reset_agent_async().await.err(),
    };

    BackgroundActionResult {
        snapshot: query_service_state_snapshot_async().await,
        error,
    }
}

pub(super) fn background_action_error_title(action: &BackgroundAction) -> &'static str {
    match action {
        BackgroundAction::RefreshStatus => "Failed to refresh playit tray",
        BackgroundAction::StartService => "Failed to start playitd service",
        BackgroundAction::StopService => "Failed to stop playitd service",
        BackgroundAction::ResetAgent => "Failed to reset playit agent",
    }
}

async fn reset_agent_async() -> Result<(), String> {
    if !query_service_running_async().await {
        return Err("playitd is not running, so Reset Agent is unavailable".to_string());
    }

    if matches!(
        query_service_lifecycle_async().await,
        Ok(AgentLifecycle::WaitingForSecret)
    ) {
        return Err(
            "playitd is already waiting for setup, so Reset Agent is unavailable".to_string(),
        );
    }

    let mut client = IpcClient::connect()
        .await
        .map_err(|error| format!("Failed to connect to playitd over IPC: {error}"))?;

    let reset_response = client
        .reset_secret()
        .await
        .map_err(|error| format!("Failed to reset agent over IPC: {error}"))?;

    if !reset_response.accepted {
        return Err(reset_response
            .message
            .unwrap_or_else(|| "playitd rejected the reset request".to_string()));
    }
    debug_log("agent secret reset requested over IPC");

    stop_service_async().await?;
    launch_playit()
}

async fn query_service_state_snapshot_async() -> ServiceStateSnapshot {
    let service_running = query_service_running_async().await;

    ServiceStateSnapshot {
        service_running,
        reset_agent_enabled: query_reset_agent_enabled_async(service_running).await,
    }
}

async fn query_reset_agent_enabled_async(service_running: bool) -> bool {
    if !service_running {
        return false;
    }

    match query_service_lifecycle_async().await {
        Ok(AgentLifecycle::WaitingForSecret) | Ok(AgentLifecycle::Stopping) => false,
        Ok(_) | Err(_) => true,
    }
}

async fn query_service_lifecycle_async() -> Result<AgentLifecycle, String> {
    let mut client = IpcClient::connect()
        .await
        .map_err(|error| format!("Failed to connect to playitd over IPC: {error}"))?;

    client
        .lifecycle()
        .await
        .map_err(|error| format!("Failed to read playitd lifecycle over IPC: {error}"))
}

pub(super) fn remove_startup_shortcut() -> Result<(), String> {
    let shortcut_path = startup_shortcut_path()?;

    if !shortcut_path.exists() {
        return Ok(());
    }

    fs::remove_file(&shortcut_path).map_err(|error| {
        format!(
            "Failed to delete startup shortcut at {}: {error}",
            shortcut_path.display()
        )
    })
}

pub(super) fn query_service_running() -> bool {
    unsafe {
        let manager = OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return false;
        }

        let service_name = wide(INSTALLED_SERVICE_LABEL);
        let service = OpenServiceW(manager, service_name.as_ptr(), SERVICE_QUERY_STATUS);
        if service.is_null() {
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
        running
    }
}

async fn query_service_running_async() -> bool {
    match task::spawn_blocking(query_service_running).await {
        Ok(service_running) => service_running,
        Err(error) => {
            debug_log(&format!("failed to query playitd service state: {error}"));
            false
        }
    }
}

async fn stop_installed_service_async() -> Result<(), String> {
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
