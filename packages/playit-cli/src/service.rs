use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playit_platform::service::{
    ReachabilityPolicy, ServiceManagerKind, start_and_wait_until_reachable,
};

use crate::CliError;
#[cfg(target_os = "linux")]
use crate::linux;
use crate::ui::ConsoleUi;

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceManagerMode {
    None,
    Systemd,
    OpenRc,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceManagerMode {
    WindowsService,
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceManagerMode {
    Native,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstalledServiceStartState {
    AlreadyRunning,
    Started,
}

pub async fn ensure_installed_service_running_for_cli(
    console: Option<&mut ConsoleUi>,
    service_manager: ServiceManagerMode,
) -> Result<InstalledServiceStartState, CliError> {
    if IpcClient::is_running(get_default_socket_path()).await {
        return Ok(InstalledServiceStartState::AlreadyRunning);
    }

    #[cfg(target_os = "linux")]
    {
        if matches!(service_manager, ServiceManagerMode::None) {
            return Err(no_service_manager_selected_error());
        }

        let linux_manager = linux_service_manager(service_manager)
            .expect("linux service manager was checked above");

        if linux::prepare_installed_service_for_cli(console, linux_manager).await? {
            return Ok(InstalledServiceStartState::AlreadyRunning);
        }

        start_and_wait(linux_manager)
            .await
            .map_err(|error| CliError::ServiceError(format!("Failed to start service: {error}")))?;

        Ok(InstalledServiceStartState::Started)
    }

    #[cfg(not(target_os = "linux"))]
    {
        match service_manager {
            #[cfg(target_os = "windows")]
            ServiceManagerMode::WindowsService => {
                start_and_wait(ServiceManagerKind::WindowsScm)
                    .await
                    .map_err(|error| {
                        CliError::ServiceError(format!("Failed to start service: {error}"))
                    })?;
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            ServiceManagerMode::Native => {
                start_and_wait(ServiceManagerKind::MacOsLaunchd)
                    .await
                    .map_err(|error| {
                        CliError::ServiceError(format!("Failed to start service: {error}"))
                    })?;
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    Ok(InstalledServiceStartState::Started)
}

pub(crate) fn installed_service_manager(
    service_manager: ServiceManagerMode,
) -> Result<ServiceManagerKind, CliError> {
    #[cfg(target_os = "linux")]
    {
        linux_service_manager(service_manager).ok_or_else(no_service_manager_selected_error)
    }

    #[cfg(target_os = "windows")]
    {
        let _ = service_manager;
        Ok(ServiceManagerKind::WindowsScm)
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = service_manager;
        Ok(ServiceManagerKind::MacOsLaunchd)
    }
}

pub fn no_service_manager_selected_error() -> CliError {
    CliError::ServiceError(no_service_manager_selected_message())
}

fn no_service_manager_selected_message() -> String {
    let socket_path = get_default_socket_path();
    format!(
        "The playit daemon is not reachable at {socket_path}.\nplayitd must be running in the background before playit can connect to it.\n\nRun with --systemd or --openrc to let playit start the installed service, or start playitd manually and connect with --socket-path:\n  playitd --socket-path=./playit.sock --secret-path=./playit.toml\n  playit --socket-path=./playit.sock"
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn linux_service_manager(
    service_manager: ServiceManagerMode,
) -> Option<ServiceManagerKind> {
    match service_manager {
        ServiceManagerMode::None => None,
        ServiceManagerMode::Systemd => Some(ServiceManagerKind::Systemd),
        ServiceManagerMode::OpenRc => Some(ServiceManagerKind::OpenRc),
    }
}

async fn start_and_wait(
    manager: ServiceManagerKind,
) -> Result<(), playit_platform::service::ServiceError> {
    start_and_wait_until_reachable(manager, ReachabilityPolicy::default(), || {
        IpcClient::is_running(get_default_socket_path())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_manager_mode_maps_to_systemd() {
        assert_eq!(
            linux_service_manager(ServiceManagerMode::Systemd),
            Some(ServiceManagerKind::Systemd)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_manager_mode_maps_to_openrc() {
        assert_eq!(
            linux_service_manager(ServiceManagerMode::OpenRc),
            Some(ServiceManagerKind::OpenRc)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_manager_mode_none_has_no_manager() {
        assert_eq!(linux_service_manager(ServiceManagerMode::None), None);
    }
}
