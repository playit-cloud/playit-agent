use service_manager::{ServiceLabel, ServiceManager, ServiceStartCtx, ServiceStopCtx};

#[cfg(target_os = "linux")]
use crate::linux;
use playit_ipc::ipc::{IpcClient, get_default_socket_path};
#[cfg(target_os = "windows")]
use windows_sys::Win32::Foundation::{
    ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_NOT_ACTIVE,
};
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx,
    SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_CONTROL_STOP,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_STATUS, SERVICE_STATUS_PROCESS,
    SERVICE_STOP, StartServiceW,
};

#[cfg(target_os = "windows")]
pub const INSTALLED_SERVICE_LABEL: &str = "playitd";

#[cfg(not(target_os = "windows"))]
pub const INSTALLED_SERVICE_LABEL: &str = "gg.playit.playitd";

#[derive(Debug)]
pub enum ServiceManagerError {
    NotAvailable(String),
    StartFailed(String),
    StopFailed(String),
    NotFound,
    IoError(std::io::Error),
}

impl std::fmt::Display for ServiceManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAvailable(msg) => write!(f, "Service manager not available: {msg}"),
            Self::StartFailed(msg) => write!(f, "Failed to start service: {msg}"),
            Self::StopFailed(msg) => write!(f, "Failed to stop service: {msg}"),
            Self::NotFound => write!(f, "Service not found"),
            Self::IoError(e) => write!(f, "IO error: {e}"),
        }
    }
}

impl std::error::Error for ServiceManagerError {}

impl From<std::io::Error> for ServiceManagerError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}

pub struct ServiceController {
    manager: Box<dyn ServiceManager>,
    label: ServiceLabel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledServiceState {
    Running,
    Stopped,
    Unknown,
}

impl ServiceController {
    const SERVICE_LABEL: &'static str = INSTALLED_SERVICE_LABEL;

    pub fn new() -> Result<Self, ServiceManagerError> {
        let manager = <dyn ServiceManager>::native()
            .map_err(|e| ServiceManagerError::NotAvailable(e.to_string()))?;
        let label = Self::SERVICE_LABEL.parse().unwrap();
        Ok(Self { manager, label })
    }

    pub fn start(&self) -> Result<(), ServiceManagerError> {
        self.manager
            .start(ServiceStartCtx {
                label: self.label.clone(),
            })
            .map_err(|e| ServiceManagerError::StartFailed(e.to_string()))
    }

    pub fn stop(&self) -> Result<(), ServiceManagerError> {
        self.manager
            .stop(ServiceStopCtx {
                label: self.label.clone(),
            })
            .map_err(|e| ServiceManagerError::StopFailed(e.to_string()))
    }
}

#[cfg(target_os = "linux")]
pub fn is_systemd_service_active() -> Result<bool, ServiceManagerError> {
    linux::is_systemd_service_active()
}

pub fn installed_service_state() -> Result<InstalledServiceState, ServiceManagerError> {
    #[cfg(target_os = "linux")]
    {
        return Ok(if linux::is_systemd_service_active()? {
            InstalledServiceState::Running
        } else {
            InstalledServiceState::Stopped
        });
    }

    #[cfg(target_os = "windows")]
    {
        return windows_installed_service_state();
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(ServiceManagerError::NotAvailable(
            "service status queries are not supported on this platform".to_string(),
        ))
    }
}

pub fn installed_service_is_running() -> Result<bool, ServiceManagerError> {
    Ok(installed_service_state()? == InstalledServiceState::Running)
}

pub async fn ensure_installed_service_running() -> Result<(), ServiceManagerError> {
    if IpcClient::is_running(get_default_socket_path()).await {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        if linux::is_systemd_service_active()? {
            return Ok(());
        }

        linux::start_systemd_service()?;
        return wait_for_installed_service().await;
    }

    #[cfg(not(target_os = "linux"))]
    #[cfg(not(target_os = "windows"))]
    {
        let controller = ServiceController::new()?;
        controller.start()?;
        wait_for_installed_service().await
    }

    #[cfg(target_os = "windows")]
    {
        windows_start_installed_service()?;
        wait_for_installed_service().await
    }
}

pub fn stop_installed_service() -> Result<(), ServiceManagerError> {
    #[cfg(target_os = "linux")]
    {
        return linux::stop_systemd_service();
    }

    #[cfg(not(target_os = "linux"))]
    #[cfg(not(target_os = "windows"))]
    {
        let controller = ServiceController::new()?;
        controller.stop()
    }

    #[cfg(target_os = "windows")]
    {
        windows_stop_installed_service()
    }
}

async fn wait_for_installed_service() -> Result<(), ServiceManagerError> {
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if IpcClient::is_running(get_default_socket_path()).await {
            return Ok(());
        }
    }

    Err(ServiceManagerError::StartFailed(
        "Service did not start within timeout".to_string(),
    ))
}

#[cfg(target_os = "windows")]
fn windows_start_installed_service() -> Result<(), ServiceManagerError> {
    unsafe {
        let manager = open_windows_service_manager()?;
        let service_name = wide(INSTALLED_SERVICE_LABEL);
        let service = OpenServiceW(
            manager.0,
            service_name.as_ptr(),
            SERVICE_START | SERVICE_QUERY_STATUS,
        );
        if service.is_null() {
            return Err(open_service_error(ServiceAction::Start));
        }

        let service = ServiceHandle(service);
        let started = StartServiceW(service.0, 0, std::ptr::null());
        if started != 0 {
            return Ok(());
        }

        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_SERVICE_ALREADY_RUNNING as i32) {
            Ok(())
        } else {
            Err(ServiceManagerError::StartFailed(error.to_string()))
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_stop_installed_service() -> Result<(), ServiceManagerError> {
    unsafe {
        let manager = open_windows_service_manager()?;
        let service_name = wide(INSTALLED_SERVICE_LABEL);
        let service = OpenServiceW(
            manager.0,
            service_name.as_ptr(),
            SERVICE_STOP | SERVICE_QUERY_STATUS,
        );
        if service.is_null() {
            return Err(open_service_error(ServiceAction::Stop));
        }

        let service = ServiceHandle(service);
        let mut status = std::mem::zeroed::<SERVICE_STATUS>();
        let stopped = ControlService(service.0, SERVICE_CONTROL_STOP, &mut status);
        if stopped != 0 {
            return Ok(());
        }

        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_SERVICE_NOT_ACTIVE as i32) {
            Ok(())
        } else {
            Err(ServiceManagerError::StopFailed(error.to_string()))
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_installed_service_state() -> Result<InstalledServiceState, ServiceManagerError> {
    unsafe {
        let manager = open_windows_service_manager()?;
        let service_name = wide(INSTALLED_SERVICE_LABEL);
        let service = OpenServiceW(manager.0, service_name.as_ptr(), SERVICE_QUERY_STATUS);
        if service.is_null() {
            return Ok(InstalledServiceState::Stopped);
        }

        let service = ServiceHandle(service);
        let mut status = std::mem::zeroed::<SERVICE_STATUS_PROCESS>();
        let mut bytes_needed = 0;
        let queried = QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut bytes_needed,
        );

        if queried == 0 {
            return Err(ServiceManagerError::NotAvailable(format!(
                "failed to query Windows service status: {}",
                std::io::Error::last_os_error()
            )));
        }

        Ok(if status.dwCurrentState == SERVICE_RUNNING {
            InstalledServiceState::Running
        } else {
            InstalledServiceState::Stopped
        })
    }
}

#[cfg(target_os = "windows")]
fn open_windows_service_manager() -> Result<ServiceHandle, ServiceManagerError> {
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(ServiceManagerError::NotAvailable(format!(
            "failed to open Windows service manager: {}",
            std::io::Error::last_os_error()
        )));
    }

    Ok(ServiceHandle(manager))
}

#[cfg(target_os = "windows")]
enum ServiceAction {
    Start,
    Stop,
}

#[cfg(target_os = "windows")]
fn open_service_error(action: ServiceAction) -> ServiceManagerError {
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
        return ServiceManagerError::NotFound;
    }

    let message = format!("failed to open Windows service: {error}");
    match action {
        ServiceAction::Start => ServiceManagerError::StartFailed(message),
        ServiceAction::Stop => ServiceManagerError::StopFailed(message),
    }
}

#[cfg(target_os = "windows")]
struct ServiceHandle(SC_HANDLE);

#[cfg(target_os = "windows")]
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
fn wide(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
