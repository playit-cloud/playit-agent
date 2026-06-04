use playit_ipc::ipc::{IpcClient, get_default_socket_path};
#[cfg(not(target_os = "linux"))]
use playitd::manager::{
    InstalledServiceState as ManagerInstalledServiceState, ensure_installed_service_running,
    installed_service_state, stop_installed_service,
};
#[cfg(target_os = "linux")]
use playitd::manager::{
    LinuxServiceManager, ensure_installed_service_running_with_linux_manager,
    stop_installed_service_with_linux_manager,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstalledServiceStopState {
    AlreadyStopped,
    StopRequested,
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

        ensure_installed_service_running_with_linux_manager(linux_manager)
            .await
            .map_err(|error| CliError::ServiceError(format!("Failed to start service: {error}")))?;

        return Ok(InstalledServiceStartState::Started);
    }

    #[cfg(not(target_os = "linux"))]
    {
        match service_manager {
            #[cfg(target_os = "windows")]
            ServiceManagerMode::WindowsService => {
                ensure_installed_service_running().await.map_err(|error| {
                    CliError::ServiceError(format!("Failed to start service: {error}"))
                })?;
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            ServiceManagerMode::Native => {
                ensure_installed_service_running().await.map_err(|error| {
                    CliError::ServiceError(format!("Failed to start service: {error}"))
                })?;
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    Ok(InstalledServiceStartState::Started)
}

pub fn stop_installed_service_for_cli(
    service_manager: ServiceManagerMode,
) -> Result<InstalledServiceStopState, CliError> {
    #[cfg(target_os = "linux")]
    {
        let Some(linux_manager) = linux_service_manager(service_manager) else {
            return Err(no_service_manager_selected_error());
        };

        if !installed_service_is_active_for_cli(service_manager)? {
            println!("The playit service is already stopped.");
            return Ok(InstalledServiceStopState::AlreadyStopped);
        }

        if let Err(error) = stop_installed_service_with_linux_manager(linux_manager) {
            tracing::warn!("Failed to stop installed service: {error}");
        }

        return Ok(InstalledServiceStopState::StopRequested);
    }

    #[cfg(not(target_os = "linux"))]
    {
        match service_manager {
            #[cfg(target_os = "windows")]
            ServiceManagerMode::WindowsService => {
                if let Err(error) = stop_installed_service() {
                    tracing::warn!("Failed to stop installed service: {error}");
                }
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            ServiceManagerMode::Native => {
                if let Err(error) = stop_installed_service() {
                    tracing::warn!("Failed to stop installed service: {error}");
                }
            }
        }

        Ok(InstalledServiceStopState::StopRequested)
    }
}

pub fn installed_service_is_active_for_cli(
    service_manager: ServiceManagerMode,
) -> Result<bool, CliError> {
    #[cfg(target_os = "linux")]
    {
        let Some(linux_manager) = linux_service_manager(service_manager) else {
            return Err(no_service_manager_selected_error());
        };

        return linux::installed_service_is_active(linux_manager);
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = service_manager;
        installed_service_state()
            .map(|state| state == ManagerInstalledServiceState::Running)
            .map_err(|error| {
                CliError::ServiceError(format!("Failed to check service status: {error}"))
            })
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
) -> Option<LinuxServiceManager> {
    match service_manager {
        ServiceManagerMode::None => None,
        ServiceManagerMode::Systemd => Some(LinuxServiceManager::Systemd),
        ServiceManagerMode::OpenRc => Some(LinuxServiceManager::OpenRc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_manager_mode_maps_to_systemd() {
        assert_eq!(
            linux_service_manager(ServiceManagerMode::Systemd),
            Some(LinuxServiceManager::Systemd)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_manager_mode_maps_to_openrc() {
        assert_eq!(
            linux_service_manager(ServiceManagerMode::OpenRc),
            Some(LinuxServiceManager::OpenRc)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_manager_mode_none_has_no_manager() {
        assert_eq!(linux_service_manager(ServiceManagerMode::None), None);
    }
}
