//! When no background service is installed (for example the standalone playit.exe
//! download), run the daemon inside this process. Tunnels stay online while the window
//! is open, and the secret lives in %LOCALAPPDATA%\playit_gg like playit 0.17.

use std::time::Duration;

use playit_ipc::ipc::IpcClient;
use playitd::manager::{InstalledServiceState, installed_service_state};
use playitd::{DaemonOptions, VersionDetails};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::CliError;
use crate::client::CliTarget;
use crate::ui::ConsoleUi;

pub fn service_is_missing() -> bool {
    matches!(
        installed_service_state(),
        Ok(InstalledServiceState::NotInstalled)
    )
}

pub struct PortableDaemon {
    socket_path: String,
    shutdown: CancellationToken,
    _instance: playitd::windows::SingleInstance,
    _handle: JoinHandle<Result<(), playitd::daemon::DaemonError>>,
}

impl PortableDaemon {
    pub async fn start(console: &mut ConsoleUi) -> Result<Self, CliError> {
        // Two windows would run the same agent twice with one secret file.
        let instance = playitd::windows::SingleInstance::acquire("Local\\playit-portable")
            .map_err(|error| CliError::ServiceError(format!("Could not start playit: {error}")))?
            .ok_or_else(|| {
                CliError::ServiceError(
                    "playit is already running in another window. Use that window, or close it and try again."
                        .to_string(),
                )
            })?;

        console
            .write_screen(
                "The playit background service is not installed, so playit is running in this window.\nYour tunnels stay online while this window is open.",
            )
            .await;

        let socket_path = format!(r"\\.\pipe\playit-portable-{}", std::process::id());
        let shutdown = CancellationToken::new();
        let options = DaemonOptions {
            secret: None,
            secret_path: Some(playitd::windows_portable_secret_path()),
            socket_path: Some(socket_path.clone()),
            log_path: Some(playitd::windows_portable_log_path()),
            platform_docker: false,
            version: VersionDetails::from_cargo_package().map_err(CliError::ServiceError)?,
            shutdown: Some(shutdown.clone()),
        };

        let handle = tokio::spawn(playitd::run_daemon(options));
        for _ in 0..50 {
            if IpcClient::is_running(&socket_path).await {
                return Ok(Self {
                    socket_path,
                    shutdown,
                    _instance: instance,
                    _handle: handle,
                });
            }
            if handle.is_finished() {
                let reason = match handle.await {
                    Ok(Err(error)) => error.to_string(),
                    Ok(Ok(())) => "it exited immediately".to_string(),
                    Err(error) => error.to_string(),
                };
                return Err(CliError::ServiceError(format!(
                    "Could not start playit: {reason}"
                )));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Err(CliError::ServiceError(
            "playit did not start within 5 seconds.".to_string(),
        ))
    }

    pub fn target(&self) -> CliTarget {
        CliTarget::Portable(self.socket_path.clone())
    }
}

impl Drop for PortableDaemon {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}
