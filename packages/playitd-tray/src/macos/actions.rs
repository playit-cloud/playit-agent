use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::Duration;

use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playit_ipc::model::AgentLifecycle;
use playitd::manager::{
    InstalledServiceState, ensure_installed_service_running, installed_service_state,
    stop_installed_service,
};
use tao::event_loop::EventLoopProxy;
use tokio::task;

use super::app::UserEvent;
use super::util::log;
use crate::presentation::ServiceView;

const DASHBOARD_URL: &str = "https://playit.gg/account/tunnels";
const UNINSTALL_SCRIPT: &str = "../Resources/uninstall.sh";

#[derive(Clone, Copy, Debug)]
pub(super) enum Request {
    RefreshStatus,
    StartService,
    StopService,
    ResetAgent,
}

#[derive(Debug)]
pub(super) struct Response {
    pub(super) request: Request,
    pub(super) view: ServiceView,
    pub(super) error: Option<String>,
}

/// Runs service actions off the main thread and posts each result back to the event loop.
pub(super) struct Backend {
    request_tx: SyncSender<Request>,
}

impl Backend {
    pub(super) fn spawn(proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        let (request_tx, request_rx) = std::sync::mpsc::sync_channel::<Request>(8);
        std::thread::Builder::new()
            .name("playitd-tray-backend".to_string())
            .spawn(move || run_backend(request_rx, proxy))
            .map_err(|error| format!("Failed to start the menu bar backend: {error}"))?;
        Ok(Self { request_tx })
    }

    /// Returns false when the queue is full.
    pub(super) fn try_send(&self, request: Request) -> bool {
        match self.request_tx.try_send(request) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                log("menu bar backend stopped");
                false
            }
        }
    }
}

fn run_backend(request_rx: Receiver<Request>, proxy: EventLoopProxy<UserEvent>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            log(&format!(
                "failed to create the menu bar backend runtime: {error}"
            ));
            return;
        }
    };

    while let Ok(request) = request_rx.recv() {
        let response = runtime.block_on(handle_request(request));
        if proxy.send_event(UserEvent::Backend(response)).is_err() {
            break;
        }
    }
}

async fn handle_request(request: Request) -> Response {
    let error = match request {
        Request::RefreshStatus => None,
        Request::StartService => start_service().await.err(),
        Request::StopService => stop_service().await.err(),
        Request::ResetAgent => reset_agent().await.err(),
    };

    Response {
        request,
        view: query_service_view().await,
        error,
    }
}

pub(super) fn error_title(request: Request) -> &'static str {
    match request {
        Request::RefreshStatus => "Failed to refresh playit status",
        Request::StartService => "Failed to start the playit service",
        Request::StopService => "Failed to stop the playit service",
        Request::ResetAgent => "Failed to reset the playit agent",
    }
}

/// Opens playit in Terminal: setup when the agent needs it, otherwise live status.
pub(super) fn launch_playit() -> Result<(), String> {
    let cli = bundled_path("playit")?;
    Command::new("open")
        .args(["-a", "Terminal"])
        .arg(&cli)
        .status()
        .map_err(|error| format!("Could not open Terminal: {error}"))
        .and_then(|status| {
            status
                .success()
                .then_some(())
                .ok_or_else(|| format!("Could not open {} in Terminal", cli.display()))
        })
}

pub(super) fn open_dashboard() -> Result<(), String> {
    open(DASHBOARD_URL.as_ref())
}

pub(super) fn open_logs_folder() -> Result<(), String> {
    let log_dir = playitd::macos_launch_agent_log_dir();
    std::fs::create_dir_all(&log_dir)
        .map_err(|error| format!("Could not create {}: {error}", log_dir.display()))?;
    open(log_dir.as_os_str())
}

/// Runs the bundled uninstaller with an administrator prompt. It removes playit.app, so the
/// menu bar app exits when it finishes.
pub(super) fn uninstall() -> Result<(), String> {
    let script = bundled_path(UNINSTALL_SCRIPT)?;
    let script = script
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let output = Command::new("osascript")
        .arg("-e")
        .arg(format!(
            "do shell script quoted form of \"{script}\" with administrator privileges"
        ))
        .output()
        .map_err(|error| format!("Could not run the uninstaller: {error}"))?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // -128 is "User canceled." from the password prompt.
    if stderr.contains("(-128)") {
        return Err(String::new());
    }
    Err(format!("The uninstaller failed: {}", stderr.trim()))
}

async fn start_service() -> Result<(), String> {
    ensure_installed_service_running()
        .await
        .map_err(|error| error.to_string())
}

async fn stop_service() -> Result<(), String> {
    task::spawn_blocking(stop_installed_service)
        .await
        .map_err(|error| format!("Failed to stop the service: {error}"))?
        .map_err(|error| error.to_string())?;

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

    // The daemon returns to setup on its own; wait for it so Terminal opens into setup.
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

async fn query_service_view() -> ServiceView {
    let state = match task::spawn_blocking(installed_service_state).await {
        Ok(Ok(state)) => state,
        Ok(Err(error)) => return ServiceView::Unknown(error.to_string()),
        Err(error) => return ServiceView::Unknown(error.to_string()),
    };

    match state {
        InstalledServiceState::Running => match query_lifecycle().await {
            Ok(lifecycle) => ServiceView::Agent(lifecycle),
            // Running but not answering yet: the daemon is still binding its socket.
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

fn open(target: &std::ffi::OsStr) -> Result<(), String> {
    let status = Command::new("open")
        .arg(target)
        .status()
        .map_err(|error| format!("Could not run open: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("Could not open {}", target.to_string_lossy()))
}

/// A file relative to this executable's folder (playit.app/Contents/MacOS).
fn bundled_path(relative: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("Could not find the playit app: {error}"))?;
    let path = exe
        .parent()
        .map(|dir| dir.join(relative))
        .ok_or_else(|| format!("Could not find {relative}"))?;
    if path.exists() {
        Ok(path)
    } else {
        Err(format!(
            "{} is missing. Reinstall playit from https://playit.gg/download",
            path.display()
        ))
    }
}
