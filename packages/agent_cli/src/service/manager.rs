//! Service manager integration for install/uninstall/start/stop.

use service_manager::{
    ServiceInstallCtx, ServiceLabel, ServiceManager, ServiceStartCtx, ServiceStopCtx,
    ServiceUninstallCtx,
};
use std::ffi::OsString;
use std::path::PathBuf;

/// Error type for service manager operations
#[derive(Debug)]
pub enum ServiceManagerError {
    /// Service manager not available on this platform
    NotAvailable(String),
    /// Failed to install service
    InstallFailed(String),
    /// Failed to uninstall service
    UninstallFailed(String),
    /// Failed to start service
    StartFailed(String),
    /// Failed to stop service
    StopFailed(String),
    /// Service not found
    NotFound,
    /// Generic IO error
    IoError(std::io::Error),
}

impl std::fmt::Display for ServiceManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceManagerError::NotAvailable(msg) => {
                write!(f, "Service manager not available: {}", msg)
            }
            ServiceManagerError::InstallFailed(msg) => {
                write!(f, "Failed to install service: {}", msg)
            }
            ServiceManagerError::UninstallFailed(msg) => {
                write!(f, "Failed to uninstall service: {}", msg)
            }
            ServiceManagerError::StartFailed(msg) => write!(f, "Failed to start service: {}", msg),
            ServiceManagerError::StopFailed(msg) => write!(f, "Failed to stop service: {}", msg),
            ServiceManagerError::NotFound => write!(f, "Service not found"),
            ServiceManagerError::IoError(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for ServiceManagerError {}

impl From<std::io::Error> for ServiceManagerError {
    fn from(e: std::io::Error) -> Self {
        ServiceManagerError::IoError(e)
    }
}

/// Service controller for managing the playit agent service
pub struct ServiceController {
    manager: Box<dyn ServiceManager>,
    label: ServiceLabel,
    system_mode: bool,
}

impl ServiceController {
    /// Service label for playit agent
    const SERVICE_LABEL: &'static str = "gg.playit.agent";
    const USER_SERVICE_LABEL: &'static str = "gg.playit.agent.user";

    /// Create a new service controller
    pub fn new(system_mode: bool) -> Result<Self, ServiceManagerError> {
        let manager = <dyn ServiceManager>::native()
            .map_err(|e| ServiceManagerError::NotAvailable(e.to_string()))?;

        let label_str = if system_mode {
            Self::SERVICE_LABEL
        } else {
            Self::USER_SERVICE_LABEL
        };

        let label: ServiceLabel = label_str.parse().unwrap();

        Ok(ServiceController {
            manager,
            label,
            system_mode,
        })
    }

    /// Get the path to the current executable
    fn get_executable_path() -> Result<PathBuf, ServiceManagerError> {
        std::env::current_exe().map_err(ServiceManagerError::IoError)
    }

    /// Install the service
    pub fn install(&self) -> Result<(), ServiceManagerError> {
        let program = Self::get_executable_path()?;

        // Build arguments for the service
        let mut args = vec![OsString::from("run-service")];
        if !self.system_mode {
            args.push(OsString::from("--user"));
        }

        let ctx = ServiceInstallCtx {
            label: self.label.clone(),
            program,
            args,
            contents: None,
            username: None,
            working_directory: None,
            environment: None,
            autostart: true,
            restart_policy: service_manager::RestartPolicy::OnFailure {
                delay_secs: Some(5),
            },
        };

        self.manager
            .install(ctx)
            .map_err(|e| ServiceManagerError::InstallFailed(e.to_string()))?;

        Ok(())
    }

    /// Uninstall the service
    pub fn uninstall(&self) -> Result<(), ServiceManagerError> {
        let ctx = ServiceUninstallCtx {
            label: self.label.clone(),
        };

        self.manager
            .uninstall(ctx)
            .map_err(|e| ServiceManagerError::UninstallFailed(e.to_string()))?;

        Ok(())
    }

    /// Start the service
    pub fn start(&self) -> Result<(), ServiceManagerError> {
        let ctx = ServiceStartCtx {
            label: self.label.clone(),
        };

        self.manager
            .start(ctx)
            .map_err(|e| ServiceManagerError::StartFailed(e.to_string()))?;

        Ok(())
    }

    /// Stop the service
    pub fn stop(&self) -> Result<(), ServiceManagerError> {
        let ctx = ServiceStopCtx {
            label: self.label.clone(),
        };

        self.manager
            .stop(ctx)
            .map_err(|e| ServiceManagerError::StopFailed(e.to_string()))?;

        Ok(())
    }

    /// Check if the service is installed
    pub fn is_installed(&self) -> bool {
        // Try to query the service - if it fails, it's not installed
        // This is a heuristic since service-manager doesn't have a direct "is_installed" method
        true // For now, assume it might be installed
    }

    /// Get the service label
    pub fn label(&self) -> &ServiceLabel {
        &self.label
    }

    /// Check if running in system mode
    pub fn is_system_mode(&self) -> bool {
        self.system_mode
    }
}

/// Ensure the service is running, starting it if necessary
pub async fn ensure_service_running(system_mode: bool) -> Result<(), ServiceManagerError> {
    use crate::service::ipc::IpcClient;

    // First check if service is already running via IPC
    if IpcClient::is_running(system_mode).await {
        tracing::info!("Service is already running");
        return Ok(());
    }

    // Try to start via service manager first
    let service_manager_result = match ServiceController::new(system_mode) {
        Ok(controller) => {
            tracing::info!("Starting service via service manager");
            controller.start()
        }
        Err(e) => {
            tracing::error!("Service manager not available: {}", e);
            Err(e)
        }
    };

    // If service manager worked, wait for it to be ready
    if service_manager_result.is_ok() {
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if IpcClient::is_running(system_mode).await {
                tracing::info!("Service started via service manager");
                return Ok(());
            }
        }
    }

    // If service manager failed or service didn't start, spawn daemon directly
    tracing::info!("Starting daemon process directly");
    spawn_daemon_process(system_mode)?;

    // Wait for daemon to be ready
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if IpcClient::is_running(system_mode).await {
            tracing::info!("Daemon started successfully");
            return Ok(());
        }
    }

    Err(ServiceManagerError::StartFailed(
        "Service did not start within timeout".to_string(),
    ))
}

/// Spawn the daemon process directly (without service manager)
fn spawn_daemon_process(system_mode: bool) -> Result<(), ServiceManagerError> {
    let exe = std::env::current_exe().map_err(ServiceManagerError::IoError)?;

    let mut args = vec!["run-service".to_string()];
    if !system_mode {
        args.push("--user".to_string());
    }

    #[cfg(unix)]
    {
        use std::process::{Command, Stdio};

        // Spawn detached process
        Command::new(&exe)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                ServiceManagerError::StartFailed(format!("Failed to spawn daemon: {}", e))
            })?;
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};

        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const DETACHED_PROCESS: u32 = 0x00000008;

        Command::new(&exe)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
            .spawn()
            .map_err(|e| {
                ServiceManagerError::StartFailed(format!("Failed to spawn daemon: {}", e))
            })?;
    }

    Ok(())
}
