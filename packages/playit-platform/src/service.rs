use std::future::Future;
use std::process::{Command, Output};
use std::time::Duration;

use service_manager::{ServiceLabel, ServiceManager, ServiceStartCtx, ServiceStopCtx};

use crate::paths::{LINUX_SERVICE_NAME, NATIVE_SERVICE_LABEL, WINDOWS_SERVICE_NAME};

#[derive(Debug)]
pub enum ServiceError {
    NotAvailable(String),
    StartFailed(String),
    StopFailed(String),
    NotFound,
    Io(std::io::Error),
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAvailable(message) => write!(f, "Service manager not available: {message}"),
            Self::StartFailed(message) => write!(f, "Failed to start service: {message}"),
            Self::StopFailed(message) => write!(f, "Failed to stop service: {message}"),
            Self::NotFound => write!(f, "Service not found"),
            Self::Io(error) => write!(f, "IO error: {error}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<std::io::Error> for ServiceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    Stopped,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManagerKind {
    Systemd,
    OpenRc,
    MacOsLaunchd,
    WindowsScm,
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReachabilityPolicy {
    pub attempts: usize,
    pub interval: Duration,
}

impl Default for ReachabilityPolicy {
    fn default() -> Self {
        Self {
            attempts: 50,
            interval: Duration::from_millis(100),
        }
    }
}

pub const fn installed_service_label() -> &'static str {
    if cfg!(target_os = "windows") {
        WINDOWS_SERVICE_NAME
    } else {
        NATIVE_SERVICE_LABEL
    }
}

pub const fn native_service_manager() -> ServiceManagerKind {
    if cfg!(target_os = "linux") {
        ServiceManagerKind::Systemd
    } else if cfg!(target_os = "macos") {
        ServiceManagerKind::MacOsLaunchd
    } else if cfg!(target_os = "windows") {
        ServiceManagerKind::WindowsScm
    } else {
        ServiceManagerKind::Native
    }
}

pub fn start_installed_service(manager: ServiceManagerKind) -> Result<(), ServiceError> {
    match manager {
        ServiceManagerKind::Systemd => {
            run_systemctl(&systemd_start_args(), ServiceError::StartFailed)
        }
        ServiceManagerKind::OpenRc => {
            run_rc_service(&openrc_start_args(), ServiceError::StartFailed)
        }
        ServiceManagerKind::MacOsLaunchd | ServiceManagerKind::Native => {
            native_controller()?.start()
        }
        ServiceManagerKind::WindowsScm => windows_start_installed_service(),
    }
}

pub fn stop_installed_service(manager: ServiceManagerKind) -> Result<(), ServiceError> {
    match manager {
        ServiceManagerKind::Systemd => {
            run_systemctl(&systemd_stop_args(), ServiceError::StopFailed)
        }
        ServiceManagerKind::OpenRc => run_rc_service(&openrc_stop_args(), ServiceError::StopFailed),
        ServiceManagerKind::MacOsLaunchd | ServiceManagerKind::Native => {
            native_controller()?.stop()
        }
        ServiceManagerKind::WindowsScm => windows_stop_installed_service(),
    }
}

pub fn installed_service_state(manager: ServiceManagerKind) -> Result<ServiceState, ServiceError> {
    match manager {
        ServiceManagerKind::Systemd => systemd_service_state(),
        ServiceManagerKind::OpenRc => openrc_service_state(),
        ServiceManagerKind::MacOsLaunchd => macos_service_state(),
        ServiceManagerKind::WindowsScm => windows_installed_service_state(),
        ServiceManagerKind::Native => Err(ServiceError::NotAvailable(
            "service status queries are not supported on this platform".to_owned(),
        )),
    }
}

pub async fn wait_until_reachable<F, Fut>(
    policy: ReachabilityPolicy,
    mut probe: F,
) -> Result<(), ServiceError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    for attempt in 0..policy.attempts {
        if probe().await {
            return Ok(());
        }
        if attempt + 1 < policy.attempts {
            tokio::time::sleep(policy.interval).await;
        }
    }

    Err(ServiceError::StartFailed(
        "Service did not become reachable within timeout".to_owned(),
    ))
}

pub async fn start_and_wait_until_reachable<F, Fut>(
    manager: ServiceManagerKind,
    policy: ReachabilityPolicy,
    probe: F,
) -> Result<(), ServiceError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    start_installed_service(manager)?;
    wait_until_reachable(policy, probe).await
}

fn native_controller() -> Result<NativeController, ServiceError> {
    let manager = <dyn ServiceManager>::native()
        .map_err(|error| ServiceError::NotAvailable(error.to_string()))?;
    let label = installed_service_label()
        .parse::<ServiceLabel>()
        .map_err(|error| ServiceError::NotAvailable(error.to_string()))?;
    Ok(NativeController { manager, label })
}

struct NativeController {
    manager: Box<dyn ServiceManager>,
    label: ServiceLabel,
}

impl NativeController {
    fn start(&self) -> Result<(), ServiceError> {
        self.manager
            .start(ServiceStartCtx {
                label: self.label.clone(),
            })
            .map_err(|error| ServiceError::StartFailed(error.to_string()))
    }

    fn stop(&self) -> Result<(), ServiceError> {
        self.manager
            .stop(ServiceStopCtx {
                label: self.label.clone(),
            })
            .map_err(|error| ServiceError::StopFailed(error.to_string()))
    }
}

fn systemd_service_state() -> Result<ServiceState, ServiceError> {
    let args = systemd_status_args();
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|error| ServiceError::NotAvailable(format!("Failed to run systemctl: {error}")))?;

    match output.status.code() {
        Some(0) => Ok(ServiceState::Running),
        Some(3) | Some(4) => Ok(ServiceState::Stopped),
        _ => Err(ServiceError::NotAvailable(format!(
            "systemctl {} failed: {}",
            args.join(" "),
            command_output_detail(&output)
        ))),
    }
}

fn openrc_service_state() -> Result<ServiceState, ServiceError> {
    let args = openrc_status_args();
    let output = Command::new("rc-service")
        .args(args)
        .output()
        .map_err(|error| {
            ServiceError::NotAvailable(format!("Failed to run rc-service: {error}"))
        })?;
    Ok(if output.status.success() {
        ServiceState::Running
    } else {
        ServiceState::Stopped
    })
}

#[cfg(target_os = "macos")]
fn macos_service_state() -> Result<ServiceState, ServiceError> {
    let uid = crate::unix::effective_uid();
    let target = format!("gui/{uid}/{NATIVE_SERVICE_LABEL}");
    let output = Command::new("launchctl")
        .args(["print", &target])
        .output()
        .map_err(|error| ServiceError::NotAvailable(format!("Failed to run launchctl: {error}")))?;
    Ok(if output.status.success() {
        ServiceState::Running
    } else {
        ServiceState::Stopped
    })
}

#[cfg(not(target_os = "macos"))]
fn macos_service_state() -> Result<ServiceState, ServiceError> {
    Err(ServiceError::NotAvailable(
        "launchd is only available on macOS".to_owned(),
    ))
}

fn run_systemctl(args: &[&str], error: fn(String) -> ServiceError) -> Result<(), ServiceError> {
    run_command("systemctl", args, error)
}

fn systemd_start_args() -> [&'static str; 2] {
    ["start", LINUX_SERVICE_NAME]
}

fn systemd_status_args() -> [&'static str; 3] {
    ["is-active", "--quiet", LINUX_SERVICE_NAME]
}

fn systemd_stop_args() -> [&'static str; 2] {
    ["stop", LINUX_SERVICE_NAME]
}

fn openrc_start_args() -> [&'static str; 2] {
    [LINUX_SERVICE_NAME, "start"]
}

fn openrc_status_args() -> [&'static str; 2] {
    [LINUX_SERVICE_NAME, "status"]
}

fn openrc_stop_args() -> [&'static str; 2] {
    [LINUX_SERVICE_NAME, "stop"]
}

fn run_rc_service(args: &[&str], error: fn(String) -> ServiceError) -> Result<(), ServiceError> {
    run_command("rc-service", args, error)
}

fn run_command(
    program: &str,
    args: &[&str],
    error: fn(String) -> ServiceError,
) -> Result<(), ServiceError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|source| error(format!("Failed to run {program}: {source}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(error(format!(
            "{program} {} failed: {}",
            args.join(" "),
            command_output_detail(&output)
        )))
    }
}

fn command_output_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    }
}

#[cfg(target_os = "windows")]
fn windows_start_installed_service() -> Result<(), ServiceError> {
    windows_scm::start()
}

#[cfg(not(target_os = "windows"))]
fn windows_start_installed_service() -> Result<(), ServiceError> {
    Err(ServiceError::NotAvailable(
        "Windows SCM is only available on Windows".to_owned(),
    ))
}

#[cfg(target_os = "windows")]
fn windows_stop_installed_service() -> Result<(), ServiceError> {
    windows_scm::stop()
}

#[cfg(not(target_os = "windows"))]
fn windows_stop_installed_service() -> Result<(), ServiceError> {
    Err(ServiceError::NotAvailable(
        "Windows SCM is only available on Windows".to_owned(),
    ))
}

#[cfg(target_os = "windows")]
fn windows_installed_service_state() -> Result<ServiceState, ServiceError> {
    windows_scm::state()
}

#[cfg(not(target_os = "windows"))]
fn windows_installed_service_state() -> Result<ServiceState, ServiceError> {
    Err(ServiceError::NotAvailable(
        "Windows SCM is only available on Windows".to_owned(),
    ))
}

#[cfg(target_os = "windows")]
mod windows_scm {
    use super::{ServiceError, ServiceState};
    use crate::paths::WINDOWS_SERVICE_NAME;
    use windows_sys::Win32::Foundation::{
        ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_NOT_ACTIVE,
    };
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx,
        SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_CONTROL_STOP,
        SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_STATUS,
        SERVICE_STATUS_PROCESS, SERVICE_STOP, StartServiceW,
    };

    pub(super) fn start() -> Result<(), ServiceError> {
        unsafe {
            let manager = open_manager()?;
            let service = OpenServiceW(
                manager.0,
                wide(WINDOWS_SERVICE_NAME).as_ptr(),
                SERVICE_START | SERVICE_QUERY_STATUS,
            );
            if service.is_null() {
                return Err(open_service_error(Action::Start));
            }
            let service = ServiceHandle(service);
            if StartServiceW(service.0, 0, std::ptr::null()) != 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_SERVICE_ALREADY_RUNNING as i32) {
                Ok(())
            } else {
                Err(ServiceError::StartFailed(error.to_string()))
            }
        }
    }

    pub(super) fn stop() -> Result<(), ServiceError> {
        unsafe {
            let manager = open_manager()?;
            let service = OpenServiceW(
                manager.0,
                wide(WINDOWS_SERVICE_NAME).as_ptr(),
                SERVICE_STOP | SERVICE_QUERY_STATUS,
            );
            if service.is_null() {
                return Err(open_service_error(Action::Stop));
            }
            let service = ServiceHandle(service);
            let mut status = std::mem::zeroed::<SERVICE_STATUS>();
            if ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) != 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_SERVICE_NOT_ACTIVE as i32) {
                Ok(())
            } else {
                Err(ServiceError::StopFailed(error.to_string()))
            }
        }
    }

    pub(super) fn state() -> Result<ServiceState, ServiceError> {
        unsafe {
            let manager = open_manager()?;
            let service = OpenServiceW(
                manager.0,
                wide(WINDOWS_SERVICE_NAME).as_ptr(),
                SERVICE_QUERY_STATUS,
            );
            if service.is_null() {
                let error = std::io::Error::last_os_error();
                return if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
                    Ok(ServiceState::Stopped)
                } else {
                    Err(ServiceError::NotAvailable(format!(
                        "failed to open Windows service: {error}"
                    )))
                };
            }
            let service = ServiceHandle(service);
            let mut status = std::mem::zeroed::<SERVICE_STATUS_PROCESS>();
            let mut needed = 0;
            if QueryServiceStatusEx(
                service.0,
                SC_STATUS_PROCESS_INFO,
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
                std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut needed,
            ) == 0
            {
                return Err(ServiceError::NotAvailable(format!(
                    "failed to query Windows service status: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(if status.dwCurrentState == SERVICE_RUNNING {
                ServiceState::Running
            } else {
                ServiceState::Stopped
            })
        }
    }

    fn open_manager() -> Result<ServiceHandle, ServiceError> {
        let manager =
            unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
        if manager.is_null() {
            Err(ServiceError::NotAvailable(format!(
                "failed to open Windows service manager: {}",
                std::io::Error::last_os_error()
            )))
        } else {
            Ok(ServiceHandle(manager))
        }
    }

    enum Action {
        Start,
        Stop,
    }

    fn open_service_error(action: Action) -> ServiceError {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
            return ServiceError::NotFound;
        }
        let message = format!("failed to open Windows service: {error}");
        match action {
            Action::Start => ServiceError::StartFailed(message),
            Action::Stop => ServiceError::StopFailed(message),
        }
    }

    struct ServiceHandle(SC_HANDLE);

    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseServiceHandle(self.0);
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_names_and_command_arguments_are_stable() {
        #[cfg(target_os = "windows")]
        assert_eq!(installed_service_label(), WINDOWS_SERVICE_NAME);
        #[cfg(not(target_os = "windows"))]
        assert_eq!(installed_service_label(), NATIVE_SERVICE_LABEL);
        assert_eq!(LINUX_SERVICE_NAME, "playit");
        assert_eq!(WINDOWS_SERVICE_NAME, "playitd");
        assert_eq!(systemd_start_args(), ["start", "playit"]);
        assert_eq!(systemd_status_args(), ["is-active", "--quiet", "playit"]);
        assert_eq!(systemd_stop_args(), ["stop", "playit"]);
        assert_eq!(openrc_start_args(), ["playit", "start"]);
        assert_eq!(openrc_status_args(), ["playit", "status"]);
        assert_eq!(openrc_stop_args(), ["playit", "stop"]);
    }

    #[tokio::test]
    async fn reachability_policy_uses_bounded_attempts() {
        let mut attempts = 0;
        wait_until_reachable(
            ReachabilityPolicy {
                attempts: 3,
                interval: Duration::ZERO,
            },
            || {
                attempts += 1;
                std::future::ready(attempts == 3)
            },
        )
        .await
        .unwrap();
        assert_eq!(attempts, 3);
    }
}
