//! Runs the daemon inside the Windows service process. There is no child process: when
//! the service stops, the daemon stops, and when the daemon fails, Windows sees a failed
//! service and applies the recovery actions configured in `service_config`.

use std::ffi::OsString;
use std::sync::Mutex;
use std::time::Duration;

use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use tokio_util::sync::CancellationToken;
use windows_service::define_windows_service;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::service_dispatcher;

use super::{legacy_secret, service_config, write_service_log};
use crate::manager::INSTALLED_SERVICE_LABEL;
use crate::{
    DaemonOptions, VersionDetails, run_daemon, windows_service_log_path,
    windows_service_secret_path,
};

const START_WAIT_HINT: Duration = Duration::from_secs(30);
const STOP_WAIT_HINT: Duration = Duration::from_secs(15);
const DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(10);

static SERVICE_OPTIONS: Mutex<Option<DaemonOptions>> = Mutex::new(None);

define_windows_service!(ffi_service_main, service_main);

/// Daemon options used by the installed service.
pub fn service_options(version: VersionDetails) -> DaemonOptions {
    DaemonOptions {
        secret: None,
        secret_path: Some(windows_service_secret_path()),
        socket_path: Some(get_default_socket_path().to_string()),
        log_path: Some(windows_service_log_path()),
        platform_docker: false,
        version,
        shutdown: None,
    }
}

/// Hands this process to the service control manager. Blocks until the service stops.
pub fn run(options: DaemonOptions) -> Result<(), String> {
    *SERVICE_OPTIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(options);

    service_dispatcher::start(INSTALLED_SERVICE_LABEL, ffi_service_main).map_err(|error| {
        format!(
            "Could not connect to the Windows service manager ({error}). `playitd --windows-service` is started by Windows; run `playit` instead."
        )
    })
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service() {
        write_service_log(&format!("service stopped with an error: {error}"));
    }
}

fn run_service() -> Result<(), String> {
    let shutdown = CancellationToken::new();
    let control_shutdown = shutdown.clone();
    let status_handle =
        service_control_handler::register(INSTALLED_SERVICE_LABEL, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                control_shutdown.cancel();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })
        .map_err(|error| format!("failed to register the service control handler: {error}"))?;

    set_status(
        &status_handle,
        ServiceState::StartPending,
        START_WAIT_HINT,
        ServiceExitCode::Win32(0),
    );

    let result = run_daemon_until_stopped(&status_handle, shutdown);
    let exit_code = match &result {
        Ok(()) => ServiceExitCode::Win32(0),
        // A non-zero service-specific code lets the recovery actions restart us.
        Err(_) => ServiceExitCode::ServiceSpecific(1),
    };
    set_status(
        &status_handle,
        ServiceState::Stopped,
        Duration::ZERO,
        exit_code,
    );
    result
}

fn run_daemon_until_stopped(
    status_handle: &ServiceStatusHandle,
    shutdown: CancellationToken,
) -> Result<(), String> {
    let Some(mut options) = SERVICE_OPTIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    else {
        return Err("service options were not set before the dispatcher started".to_string());
    };
    options.shutdown = Some(shutdown.clone());

    prepare_environment();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to create the tokio runtime: {error}"))?;

    set_status(
        status_handle,
        ServiceState::Running,
        Duration::ZERO,
        ServiceExitCode::Win32(0),
    );

    let result = runtime.block_on(async {
        stop_stale_daemon().await;

        let mut daemon = tokio::spawn(run_daemon(options));
        tokio::select! {
            result = &mut daemon => flatten_daemon_result(result),
            _ = shutdown.cancelled() => {
                set_status(
                    status_handle,
                    ServiceState::StopPending,
                    STOP_WAIT_HINT,
                    ServiceExitCode::Win32(0),
                );
                match tokio::time::timeout(DAEMON_STOP_TIMEOUT, &mut daemon).await {
                    Ok(result) => flatten_daemon_result(result),
                    Err(_) => {
                        daemon.abort();
                        write_service_log("daemon did not stop in time; aborted it");
                        Ok(())
                    }
                }
            }
        }
    });

    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

/// Best-effort setup that must happen before the daemon reads its secret. Failures are
/// logged and never stop the service from starting.
fn prepare_environment() {
    if let Err(error) = service_config::secure_data_directories() {
        write_service_log(&format!("could not secure the data directory: {error}"));
    }

    match legacy_secret::migrate_if_missing() {
        Ok(Some(source)) => write_service_log(&format!(
            "migrated the playit 0.17 secret from {}",
            source.display()
        )),
        Ok(None) => {}
        Err(error) => write_service_log(&format!("could not migrate a 0.17 secret: {error}")),
    }

    if let Err(error) = service_config::configure_installed_service() {
        write_service_log(&format!("could not configure the service: {error}"));
    }
}

/// A daemon from an older install (or one started by hand) may still own the pipe. Ask it
/// to stop so the service can take over instead of failing with "already running".
async fn stop_stale_daemon() {
    let socket_path = get_default_socket_path();
    if !IpcClient::is_running(socket_path).await {
        return;
    }

    write_service_log("another playitd owns the IPC pipe; asking it to stop");
    if let Ok(mut client) = IpcClient::connect_with_path(socket_path).await {
        let _ = client.stop().await;
    }

    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if !IpcClient::is_running(socket_path).await {
            return;
        }
    }
    write_service_log("the other playitd did not stop within 10s");
}

fn flatten_daemon_result(
    result: Result<Result<(), crate::daemon::DaemonError>, tokio::task::JoinError>,
) -> Result<(), String> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error.to_string()),
        Err(error) => Err(format!("daemon task failed: {error}")),
    }
}

fn set_status(
    status_handle: &ServiceStatusHandle,
    current_state: ServiceState,
    wait_hint: Duration,
    exit_code: ServiceExitCode,
) {
    let controls_accepted = match current_state {
        ServiceState::Running => ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        _ => ServiceControlAccept::empty(),
    };
    let checkpoint = match current_state {
        ServiceState::StartPending | ServiceState::StopPending => 1,
        _ => 0,
    };

    if let Err(error) = status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state,
        controls_accepted,
        exit_code,
        checkpoint,
        wait_hint,
        process_id: None,
    }) {
        write_service_log(&format!(
            "failed to report service state {current_state:?}: {error}"
        ));
    }
}
