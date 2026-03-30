use service_manager::{ServiceLabel, ServiceManager, ServiceStartCtx, ServiceStopCtx};

use playit_ipc::ipc::{IpcClient, get_default_socket_path};

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

impl ServiceController {
    const SERVICE_LABEL: &'static str = "gg.playit.playitd";

    #[cfg(not(target_os = "linux"))]
    pub fn new() -> Result<Self, ServiceManagerError> {
        let manager = <dyn ServiceManager>::native()
            .map_err(|e| ServiceManagerError::NotAvailable(e.to_string()))?;
        let label = Self::SERVICE_LABEL.parse().unwrap();

        Ok(Self { manager, label })
    }

    #[cfg(target_os = "linux")]
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
fn start_systemd_service() -> Result<(), ServiceManagerError> {
    run_systemctl(&["start", "playitd"], ServiceManagerError::StartFailed)
}

#[cfg(target_os = "linux")]
pub fn stop_systemd_service() -> Result<(), ServiceManagerError> {
    run_systemctl(&["stop", "playitd"], ServiceManagerError::StopFailed)
}

#[cfg(target_os = "linux")]
fn run_systemctl(
    args: &[&str],
    err_builder: fn(String) -> ServiceManagerError,
) -> Result<(), ServiceManagerError> {
    use std::process::Command;

    let output = Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| err_builder(format!("Failed to run systemctl: {e}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(err_builder(format!(
        "systemctl {} failed: {}",
        args.join(" "),
        stderr
    )))
}

pub async fn ensure_installed_service_running() -> Result<(), ServiceManagerError> {
    if IpcClient::is_running(get_default_socket_path()).await {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        start_systemd_service()?;
        return wait_for_installed_service().await;
    }

    #[cfg(not(target_os = "linux"))]
    {
        let controller = ServiceController::new()?;
        controller.start()?;
        wait_for_installed_service().await
    }
}

pub fn stop_installed_service() -> Result<(), ServiceManagerError> {
    #[cfg(target_os = "linux")]
    {
        return stop_systemd_service();
    }

    #[cfg(not(target_os = "linux"))]
    {
        let controller = ServiceController::new()?;
        controller.stop()
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
