#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod windows_service_host {
    use std::ffi::OsString;
    use std::fs;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc::{self, Receiver};
    use std::time::{Duration, Instant};

    use playit_ipc::ipc::{IpcClient, get_default_socket_path};
    use playitd::manager::INSTALLED_SERVICE_LABEL;
    use playitd::{windows_service_log_path, windows_service_secret_path};
    use std::os::windows::process::CommandExt;
    use windows_service::define_windows_service;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{Result, service_dispatcher};
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    const START_TIMEOUT: Duration = Duration::from_secs(15);
    const STOP_TIMEOUT: Duration = Duration::from_secs(15);

    define_windows_service!(ffi_service_main, service_main);

    pub fn run() -> Result<()> {
        service_dispatcher::start(INSTALLED_SERVICE_LABEL, ffi_service_main)
    }

    fn service_main(_arguments: Vec<OsString>) {
        if let Err(error) = run_service() {
            eprintln!("playitd-service error: {error}");
        }
    }

    fn run_service() -> Result<()> {
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let status_handle =
            service_control_handler::register(INSTALLED_SERVICE_LABEL, move |control_event| {
                match control_event {
                    ServiceControl::Stop | ServiceControl::Shutdown => {
                        let _ = shutdown_tx.send(());
                        ServiceControlHandlerResult::NoError
                    }
                    ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                    _ => ServiceControlHandlerResult::NotImplemented,
                }
            })?;

        set_service_status(
            &status_handle,
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            ServiceExitCode::Win32(0),
            START_TIMEOUT,
        )?;

        let mut child = match spawn_daemon_process() {
            Ok(child) => child,
            Err(error) => {
                eprintln!("failed to spawn playitd: {error}");
                set_service_status(
                    &status_handle,
                    ServiceState::Stopped,
                    ServiceControlAccept::empty(),
                    ServiceExitCode::Win32(1),
                    Duration::default(),
                )?;
                return Ok(());
            }
        };

        match wait_for_startup_ready(&mut child, START_TIMEOUT) {
            StartupState::Ready => {}
            StartupState::Exited(exit_code) => {
                set_service_status(
                    &status_handle,
                    ServiceState::Stopped,
                    ServiceControlAccept::empty(),
                    ServiceExitCode::Win32(exit_code),
                    Duration::default(),
                )?;
                return Ok(());
            }
            StartupState::TimedOut => {
                let exit_code = terminate_child(&mut child);
                set_service_status(
                    &status_handle,
                    ServiceState::Stopped,
                    ServiceControlAccept::empty(),
                    ServiceExitCode::Win32(exit_code),
                    Duration::default(),
                )?;
                return Ok(());
            }
        }

        set_service_status(
            &status_handle,
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            ServiceExitCode::Win32(0),
            Duration::default(),
        )?;

        let exit_code = wait_for_stop_or_exit(&status_handle, &mut child, shutdown_rx)?;

        set_service_status(
            &status_handle,
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            ServiceExitCode::Win32(exit_code),
            Duration::default(),
        )?;

        Ok(())
    }

    fn wait_for_stop_or_exit(
        status_handle: &service_control_handler::ServiceStatusHandle,
        child: &mut Child,
        shutdown_rx: Receiver<()>,
    ) -> Result<u32> {
        loop {
            if let Some(exit_status) = child.try_wait().ok().flatten() {
                return Ok(exit_code_from_status(exit_status.code()));
            }

            match shutdown_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    set_service_status(
                        status_handle,
                        ServiceState::StopPending,
                        ServiceControlAccept::empty(),
                        ServiceExitCode::Win32(0),
                        STOP_TIMEOUT,
                    )?;
                    request_child_shutdown();
                    return Ok(wait_for_child_exit(child, STOP_TIMEOUT));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn spawn_daemon_process() -> std::io::Result<Child> {
        let secret_path = windows_service_secret_path();
        let log_path = windows_service_log_path();

        if let Some(parent) = secret_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let daemon_path = std::env::current_exe()?.with_file_name("playitd.exe");
        Command::new(daemon_path)
            .arg("--socket-path")
            .arg(get_default_socket_path())
            .arg("--secret-path")
            .arg(secret_path)
            .arg("--log-path")
            .arg(log_path)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }

    enum StartupState {
        Ready,
        Exited(u32),
        TimedOut,
    }

    fn wait_for_startup_ready(child: &mut Child, timeout: Duration) -> StartupState {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return StartupState::TimedOut;
        };

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(exit_status)) => {
                    return StartupState::Exited(exit_code_from_status(exit_status.code()));
                }
                Ok(None) => {}
                Err(_) => return StartupState::Exited(1),
            }

            if runtime.block_on(IpcClient::is_running(get_default_socket_path())) {
                return StartupState::Ready;
            }

            std::thread::sleep(Duration::from_millis(200));
        }

        StartupState::TimedOut
    }

    fn request_child_shutdown() {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };

        runtime.block_on(async {
            if let Ok(mut client) = IpcClient::connect_with_path(get_default_socket_path()).await {
                let _ = client.stop().await;
            }
        });
    }

    fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> u32 {
        let deadline = Instant::now() + timeout;

        loop {
            match child.try_wait() {
                Ok(Some(exit_status)) => return exit_code_from_status(exit_status.code()),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Ok(None) => {
                    let _ = child.kill();
                    break;
                }
                Err(_) => return 1,
            }
        }

        child
            .wait()
            .map(|exit_status| exit_code_from_status(exit_status.code()))
            .unwrap_or(1)
    }

    fn terminate_child(child: &mut Child) -> u32 {
        let _ = child.kill();
        child
            .wait()
            .map(|exit_status| exit_code_from_status(exit_status.code()))
            .unwrap_or(1)
    }

    fn exit_code_from_status(code: Option<i32>) -> u32 {
        match code {
            Some(code) if code >= 0 => code as u32,
            _ => 1,
        }
    }

    fn set_service_status(
        status_handle: &service_control_handler::ServiceStatusHandle,
        current_state: ServiceState,
        controls_accepted: ServiceControlAccept,
        exit_code: ServiceExitCode,
        wait_hint: Duration,
    ) -> Result<()> {
        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state,
            controls_accepted,
            exit_code,
            checkpoint: 0,
            wait_hint,
            process_id: None,
        })
    }
}

#[cfg(target_os = "windows")]
fn main() -> windows_service::Result<()> {
    windows_service_host::run()
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("playitd-service is only supported on Windows");
    std::process::exit(1);
}
