use std::ffi::OsStr;
use std::ffi::c_void;
use std::fs;
use std::iter::once;
use std::mem::zeroed;
use std::os::windows::ffi::OsStrExt;
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
use windows_sys::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT,
    SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
};
use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;
use windows_sys::Win32::UI::Shell::{FOLDERID_Startup, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows_sys::core::{GUID, HRESULT, PCWSTR};

use super::protocol::{BackendRequest, BackendRequestKind, BackendResponse, ServiceStateSnapshot};
use super::util::{debug_log, wide};

const TRAY_SHORTCUT_NAME: &str = "Playit Tray.lnk";
const TRAY_SHORTCUT_DESCRIPTION: &str =
    "Shows the Playit tray icon when the background service is running.";
const CLSID_SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
const IID_ISHELL_LINK_W: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
const IID_IPERSIST_FILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);

#[repr(C)]
struct IUnknown {
    vtable: *const IUnknownVTable,
}

#[repr(C)]
struct IUnknownVTable {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct IPersistVTable {
    parent: IUnknownVTable,
    get_class_id: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT,
}

#[repr(C)]
struct IPersistFile {
    vtable: *const IPersistFileVTable,
}

#[repr(C)]
struct IPersistFileVTable {
    parent: IPersistVTable,
    is_dirty: unsafe extern "system" fn(*mut IPersistFile) -> HRESULT,
    load: unsafe extern "system" fn(*mut IPersistFile, PCWSTR, u32) -> HRESULT,
    save: unsafe extern "system" fn(*mut IPersistFile, PCWSTR, i32) -> HRESULT,
    save_completed: unsafe extern "system" fn(*mut IPersistFile, PCWSTR) -> HRESULT,
    get_cur_file: unsafe extern "system" fn(*mut IPersistFile, *mut *mut u16) -> HRESULT,
}

#[repr(C)]
struct IShellLinkW {
    vtable: *const IShellLinkWVTable,
}

#[repr(C)]
struct IShellLinkWVTable {
    parent: IUnknownVTable,
    get_path:
        unsafe extern "system" fn(*mut IShellLinkW, *mut u16, i32, *mut c_void, u32) -> HRESULT,
    get_id_list: unsafe extern "system" fn(*mut IShellLinkW, *mut *mut c_void) -> HRESULT,
    set_id_list: unsafe extern "system" fn(*mut IShellLinkW, *const c_void) -> HRESULT,
    get_description: unsafe extern "system" fn(*mut IShellLinkW, *mut u16, i32) -> HRESULT,
    set_description: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR) -> HRESULT,
    get_working_directory: unsafe extern "system" fn(*mut IShellLinkW, *mut u16, i32) -> HRESULT,
    set_working_directory: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR) -> HRESULT,
    get_arguments: unsafe extern "system" fn(*mut IShellLinkW, *mut u16, i32) -> HRESULT,
    set_arguments: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR) -> HRESULT,
    get_hotkey: unsafe extern "system" fn(*mut IShellLinkW, *mut u16) -> HRESULT,
    set_hotkey: unsafe extern "system" fn(*mut IShellLinkW, u16) -> HRESULT,
    get_show_cmd: unsafe extern "system" fn(*mut IShellLinkW, *mut i32) -> HRESULT,
    set_show_cmd: unsafe extern "system" fn(*mut IShellLinkW, i32) -> HRESULT,
    get_icon_location:
        unsafe extern "system" fn(*mut IShellLinkW, *mut u16, i32, *mut i32) -> HRESULT,
    set_icon_location: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR, i32) -> HRESULT,
    set_relative_path: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR, u32) -> HRESULT,
    resolve: unsafe extern "system" fn(*mut IShellLinkW, *mut c_void, u32) -> HRESULT,
    set_path: unsafe extern "system" fn(*mut IShellLinkW, PCWSTR) -> HRESULT,
}

struct ComInitialization {
    should_uninitialize: bool,
}

impl Drop for ComInitialization {
    fn drop(&mut self) {
        if self.should_uninitialize {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

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

pub(super) fn ensure_startup_shortcut() -> Result<(), String> {
    let shortcut_path = startup_shortcut_path()?;
    let tray_path = std::env::current_exe()
        .map_err(|error| format!("Failed to resolve playitd-tray.exe path: {error}"))?;
    let working_directory = tray_path.parent().ok_or_else(|| {
        format!(
            "Failed to resolve the working directory for {}",
            tray_path.display()
        )
    })?;

    if let Some(parent) = shortcut_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create the current user's Startup folder at {}: {error}",
                parent.display()
            )
        })?;
    }

    debug_log(&format!(
        "ensure_startup_shortcut: writing {} -> {}",
        shortcut_path.display(),
        tray_path.display()
    ));

    let _com = initialize_com()?;
    unsafe {
        let mut shell_link_ptr = null_mut::<c_void>();
        check_hresult(
            "Failed to create the ShellLink COM object",
            CoCreateInstance(
                &CLSID_SHELL_LINK,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ISHELL_LINK_W,
                &mut shell_link_ptr,
            ),
        )?;

        let shell_link = shell_link_ptr.cast::<IShellLinkW>();
        let result =
            create_startup_shortcut(shell_link, &tray_path, working_directory, &shortcut_path);
        release_com_ptr(shell_link_ptr);
        result
    }
}

pub(super) fn startup_shortcut_exists() -> Result<bool, String> {
    Ok(startup_shortcut_path()?.exists())
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
    known_folder_shortcut_path(&FOLDERID_Startup, "the current user's Startup folder")
}

fn known_folder_shortcut_path(folder_id: &GUID, folder_name: &str) -> Result<PathBuf, String> {
    unsafe {
        let mut wide_path = null_mut();
        let result = SHGetKnownFolderPath(
            folder_id,
            KF_FLAG_DEFAULT as u32,
            null_mut(),
            &mut wide_path,
        );

        if result < 0 {
            return Err(format!(
                "Failed to resolve {folder_name} (HRESULT {result:#x})"
            ));
        }

        if wide_path.is_null() {
            return Err(format!("{folder_name} path was empty"));
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

fn create_startup_shortcut(
    shell_link: *mut IShellLinkW,
    tray_path: &std::path::Path,
    working_directory: &std::path::Path,
    shortcut_path: &std::path::Path,
) -> Result<(), String> {
    let tray_path_wide = wide_os(tray_path.as_os_str());
    let working_directory_wide = wide_os(working_directory.as_os_str());
    let shortcut_path_wide = wide_os(shortcut_path.as_os_str());
    let description_wide = wide(TRAY_SHORTCUT_DESCRIPTION);

    unsafe {
        check_hresult(
            &format!(
                "Failed to set the tray shortcut target to {}",
                tray_path.display()
            ),
            ((*(*shell_link).vtable).set_path)(shell_link, tray_path_wide.as_ptr()),
        )?;
        check_hresult(
            &format!(
                "Failed to set the tray shortcut working directory to {}",
                working_directory.display()
            ),
            ((*(*shell_link).vtable).set_working_directory)(
                shell_link,
                working_directory_wide.as_ptr(),
            ),
        )?;
        check_hresult(
            "Failed to set the tray shortcut description",
            ((*(*shell_link).vtable).set_description)(shell_link, description_wide.as_ptr()),
        )?;

        let persist_file =
            query_interface::<IPersistFile>(shell_link.cast::<c_void>(), &IID_IPERSIST_FILE)?;
        let result = check_hresult(
            &format!(
                "Failed to save the startup shortcut at {}",
                shortcut_path.display()
            ),
            ((*(*persist_file).vtable).save)(persist_file, shortcut_path_wide.as_ptr(), 1),
        );
        release_com_ptr(persist_file.cast::<c_void>());
        result
    }
}

fn initialize_com() -> Result<ComInitialization, String> {
    unsafe {
        let result = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        if result == S_OK || result == S_FALSE {
            return Ok(ComInitialization {
                should_uninitialize: true,
            });
        }
        if result == RPC_E_CHANGED_MODE {
            return Ok(ComInitialization {
                should_uninitialize: false,
            });
        }

        Err(format!(
            "Failed to initialize COM for the tray shortcut helper (HRESULT {result:#x})"
        ))
    }
}

fn check_hresult(context: &str, result: HRESULT) -> Result<(), String> {
    if result < 0 {
        Err(format!("{context} (HRESULT {result:#x})"))
    } else {
        Ok(())
    }
}

fn query_interface<T>(interface: *mut c_void, iid: &GUID) -> Result<*mut T, String> {
    unsafe {
        let unknown = interface.cast::<IUnknown>();
        let mut out = null_mut::<c_void>();
        check_hresult(
            "Failed to query the tray shortcut persistence interface",
            ((*(*unknown).vtable).query_interface)(interface, iid, &mut out),
        )?;
        Ok(out.cast::<T>())
    }
}

fn release_com_ptr(interface: *mut c_void) {
    if interface.is_null() {
        return;
    }

    unsafe {
        let unknown = interface.cast::<IUnknown>();
        let _ = ((*(*unknown).vtable).release)(interface);
    }
}

fn wide_os(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(once(0)).collect()
}
