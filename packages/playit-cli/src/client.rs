use std::time::Duration;
#[cfg(target_os = "linux")]
use std::{
    ffi::CStr,
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};

use chrono::{DateTime, Utc};
use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playit_ipc::model::{
    AgentLifecycle, LogLevel as ServiceLogLevel, ServicePhase, ServiceUpdate, SubscribeResponse,
};
#[cfg(target_os = "linux")]
use playitd::manager::is_systemd_service_active;
use playitd::manager::{ensure_installed_service_running, stop_installed_service};

use crate::ui::{ConnectionStats, ConsoleUi, TuiApp};
use crate::{CliError, EXE_NAME, run_setup_flow};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachMode {
    Interactive,
    Stdout,
}

enum AttachErrorContext {
    Standard,
    AutoCommand {
        start_attempt_failed: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstalledServiceStartState {
    AlreadyRunning,
    Started,
}

#[derive(Debug, Clone)]
pub enum CliTarget {
    InstalledService,
    ExplicitSocket(String),
}

impl CliTarget {
    pub fn from_socket_path(socket_path: Option<String>) -> Self {
        match socket_path {
            Some(socket_path) => Self::ExplicitSocket(socket_path),
            None => Self::InstalledService,
        }
    }

    pub fn socket_path(&self) -> &str {
        match self {
            Self::InstalledService => get_default_socket_path(),
            Self::ExplicitSocket(path) => path.as_str(),
        }
    }
}

pub async fn run_attach_command(target: &CliTarget, mode: AttachMode) -> Result<(), CliError> {
    run_attach_command_with_context(target, mode, AttachErrorContext::Standard).await
}

pub async fn run_auto_command(
    console: &mut ConsoleUi,
    target: &CliTarget,
    attach_mode: AttachMode,
) -> Result<(), CliError> {
    let start_attempt_failed = match target {
        CliTarget::InstalledService => ensure_installed_service_running_for_cli(Some(console))
            .await
            .err()
            .map(|error| error.to_string()),
        CliTarget::ExplicitSocket(_) => None,
    };

    let mut client = connect_target(target).await.map_err(|_| {
        initial_attach_error(
            target,
            &AttachErrorContext::AutoCommand {
                start_attempt_failed: start_attempt_failed.clone(),
            },
        )
    })?;

    match wait_for_auto_lifecycle(&mut client).await? {
        AgentLifecycle::Running(_) => {}
        AgentLifecycle::WaitingForSecret => {
            run_setup_flow(console, target).await?;
        }
        AgentLifecycle::HasInvalidSecret(error) => {
            let should_reset = console
                .yn_question(
                    format!(
                        "playitd has an invalid secret configuration: {}.\nReset the secret and run setup again?",
                        error.message
                    ),
                    Some(false),
                )
                .await?;

            if !should_reset {
                return Err(CliError::ServiceError(
                    "playitd has an invalid secret configuration. Run `playit reset` or rerun `playit` and confirm the reset prompt to reclaim this agent."
                        .to_string(),
                ));
            }

            reset_service_secret_for_setup(target).await?;
            wait_for_service_waiting_for_secret(target).await?;
            run_setup_flow(console, target).await?;
        }
        AgentLifecycle::Starting => {
            return Err(CliError::ServiceError(
                "Timed out waiting for playitd to finish starting".to_string(),
            ));
        }
        AgentLifecycle::Stopping => {
            return Err(CliError::ServiceError(
                "playitd is stopping and cannot be auto-attached right now".to_string(),
            ));
        }
        AgentLifecycle::Error(error) => {
            return Err(CliError::ServiceError(format!(
                "playitd reported an error and cannot continue auto mode: {}",
                error.message
            )));
        }
    }

    run_attach_command_with_context(
        target,
        attach_mode,
        AttachErrorContext::AutoCommand {
            start_attempt_failed,
        },
    )
    .await
}

async fn wait_for_auto_lifecycle(client: &mut IpcClient) -> Result<AgentLifecycle, CliError> {
    for _ in 0..50 {
        let lifecycle = client.lifecycle().await.map_err(|error| {
            CliError::IpcError(format!("Failed to read playitd lifecycle: {error}"))
        })?;

        if !matches!(lifecycle, AgentLifecycle::Starting) {
            return Ok(lifecycle);
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Ok(AgentLifecycle::Starting)
}

async fn reset_service_secret_for_setup(target: &CliTarget) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let response = client
        .reset_secret()
        .await
        .map_err(|error| CliError::IpcError(format!("Failed to reset secret: {error}")))?;

    if !response.accepted {
        return Err(CliError::IpcError(response.message.unwrap_or_else(|| {
            "playitd rejected the reset request".to_string()
        })));
    }

    Ok(())
}

async fn wait_for_service_waiting_for_secret(target: &CliTarget) -> Result<(), CliError> {
    for _ in 0..50 {
        let mut client = match connect_target(target).await {
            Ok(client) => client,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        let lifecycle = client.lifecycle().await.map_err(|error| {
            CliError::IpcError(format!("Failed to read playitd lifecycle: {error}"))
        })?;

        match lifecycle {
            AgentLifecycle::WaitingForSecret => return Ok(()),
            AgentLifecycle::HasInvalidSecret(_)
            | AgentLifecycle::Running(_)
            | AgentLifecycle::Starting => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            AgentLifecycle::Stopping => {
                return Err(CliError::ServiceError(
                    "playitd is stopping and did not become ready for setup after reset"
                        .to_string(),
                ));
            }
            AgentLifecycle::Error(error) => {
                return Err(CliError::ServiceError(format!(
                    "playitd reported an error after reset: {}",
                    error.message
                )));
            }
        }
    }

    Err(CliError::ServiceError(
        "Timed out waiting for playitd to become ready for setup after reset".to_string(),
    ))
}

async fn run_attach_command_with_context(
    target: &CliTarget,
    mode: AttachMode,
    error_context: AttachErrorContext,
) -> Result<(), CliError> {
    let mut client = connect_target(target)
        .await
        .map_err(|_| initial_attach_error(target, &error_context))?;

    let subscribe = client
        .subscribe()
        .await
        .map_err(|_| initial_attach_error(target, &error_context))?;

    match mode {
        AttachMode::Interactive => run_attach_tui_session(client, target, subscribe).await,
        AttachMode::Stdout => run_attach_stdout_session(client, target).await,
    }
}

async fn run_attach_tui_session(
    mut client: IpcClient,
    target: &CliTarget,
    subscribe: SubscribeResponse,
) -> Result<(), CliError> {
    let mut tui = TuiApp::new();
    tui.apply_status(subscribe.snapshot.status);
    tui.apply_lifecycle(subscribe.snapshot.lifecycle);
    tui.set_stats(ConnectionStats::from(subscribe.snapshot.stats));

    let _close_guard = crate::signal_handle::get_signal_handle().close_guard();

    loop {
        tokio::select! {
            update_result = client.recv_update() => {
                match update_result {
                    Ok(update) => apply_tui_update(&mut tui, update),
                    Err(error) => {
                        tui.shutdown()?;
                        println!("{}", attach_lost_message(target, &error.to_string()));
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                match tui.tick() {
                    Ok(true) => {}
                    Ok(false) => {
                        tui.shutdown()?;
                        print_detach_message();
                        break;
                    }
                    Err(error) => {
                        tui.shutdown()?;
                        return Err(error);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn run_attach_stdout_session(
    mut client: IpcClient,
    target: &CliTarget,
) -> Result<(), CliError> {
    loop {
        tokio::select! {
            update_result = client.recv_update() => {
                match update_result {
                    Ok(update) => apply_stdout_update(update),
                    Err(error) => {
                        eprintln!("{}", attach_lost_message(target, &error.to_string()));
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!();
                print_detach_message();
                break;
            }
        }
    }

    Ok(())
}

fn apply_tui_update(tui: &mut TuiApp, update: ServiceUpdate) {
    match update {
        ServiceUpdate::Lifecycle(state) => tui.apply_lifecycle(state),
        ServiceUpdate::Status(status) => tui.apply_status(status),
        ServiceUpdate::Stats(stats) => tui.set_stats(stats.into()),
        ServiceUpdate::Log(entry) => tui.push_service_log(entry),
    }
}

fn apply_stdout_update(update: ServiceUpdate) {
    if let ServiceUpdate::Log(entry) = update {
        println!(
            "{} {:>5} {}: {}",
            format_timestamp_millis(entry.timestamp),
            format_log_level(&entry.level),
            entry.target,
            entry.message
        );
    }
}

fn print_detach_message() {
    println!("Detached from service. Service continues running in background.");
    println!("Use '{} stop' to stop the service.", *EXE_NAME);
}

pub async fn run_start_command(
    console: &mut ConsoleUi,
    target: &CliTarget,
) -> Result<(), CliError> {
    if let CliTarget::ExplicitSocket(path) = target {
        return Err(CliError::ServiceError(format!(
            "The start command only manages the installed playitd service and cannot be used with --socket-path ({path})."
        )));
    }

    match ensure_installed_service_running_for_cli(Some(console)).await? {
        InstalledServiceStartState::AlreadyRunning => {
            println!("playitd service is already running")
        }
        InstalledServiceStartState::Started => println!("playitd service started"),
    }
    println!("Run \"playit attach\" to see the playit program.");
    Ok(())
}

pub async fn run_stop_command(target: &CliTarget) -> Result<(), CliError> {
    match target {
        CliTarget::InstalledService => {
            match connect_target(target).await {
                Ok(mut client) => match client.stop().await {
                    Ok(response) if response.accepted => {
                        println!("playitd service stop requested");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    Ok(response) => {
                        tracing::warn!(
                            "playitd rejected stop request: {}",
                            response
                                .message
                                .unwrap_or_else(|| "service rejected stop request".to_string())
                        );
                    }
                    Err(error) => {
                        tracing::warn!("Failed to send stop via IPC: {error}");
                        eprintln!(
                            "Could not reach playitd over IPC, attempting to stop the installed service directly."
                        );
                    }
                },
                Err(error) => {
                    tracing::warn!("Failed to connect to installed service over IPC: {error}");
                    eprintln!(
                        "Could not reach playitd over IPC, attempting to stop the installed service directly."
                    );
                }
            }

            if let Err(error) = stop_installed_service() {
                tracing::warn!("Failed to stop installed service: {error}");
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
            if !IpcClient::is_running(get_default_socket_path()).await {
                println!("playitd service stopped");
            } else {
                println!("playitd service may still be running");
            }

            Ok(())
        }
        CliTarget::ExplicitSocket(path) => {
            let mut client = connect_target(target).await?;
            let response = client.stop().await.map_err(|error| {
                CliError::IpcError(format!("Failed to stop daemon at {path}: {error}"))
            })?;

            if !response.accepted {
                return Err(CliError::IpcError(response.message.unwrap_or_else(|| {
                    format!("playitd rejected stop request for {path}")
                })));
            }

            println!("playitd stop requested for socket {path}");
            tokio::time::sleep(Duration::from_secs(1)).await;

            if !IpcClient::is_running(path.as_str()).await {
                println!("playitd daemon stopped");
            } else {
                println!("playitd daemon may still be running");
            }

            Ok(())
        }
    }
}

pub async fn run_status_command(target: &CliTarget) -> Result<(), CliError> {
    if !IpcClient::is_running(target.socket_path()).await {
        match target {
            CliTarget::InstalledService => println!("playitd service is not running"),
            CliTarget::ExplicitSocket(path) => {
                println!("playitd daemon is not reachable at socket {path}")
            }
        }
        return Ok(());
    }

    let mut client = connect_target(target).await?;

    match client.status().await {
        Ok(status) => {
            match target {
                CliTarget::InstalledService => println!("playitd service status:"),
                CliTarget::ExplicitSocket(path) => {
                    println!("playitd daemon status for socket {path}:")
                }
            }
            println!("  Phase: {}", format_service_phase(&status.phase));
            println!("  PID: {}", status.pid);
            println!("  Uptime: {} seconds", status.uptime_secs);
            println!("  Version: {}", status.version);
            println!("  Socket: {}", status.socket_path);
            match &status.secret_path {
                Some(secret_path) => println!("  Secret path: {}", secret_path),
                None => println!("  Secret path: <inline secret>"),
            }
            println!("  Secret configured: {}", status.has_secret);
            println!("  Protocol version: {}", status.protocol.version);
            if !status.protocol.capabilities.is_empty() {
                println!("  Capabilities: {:?}", status.protocol.capabilities);
            }
            if let Some(error) = status.last_error {
                println!("  Last error: {}", error.message);
            }
        }
        Err(_) => return Err(ipc_connection_error()),
    }

    Ok(())
}

pub async fn ensure_service_waiting_for_secret(
    console: &mut ConsoleUi,
    target: &CliTarget,
) -> Result<(), CliError> {
    if matches!(target, CliTarget::InstalledService) {
        ensure_installed_service_running_for_cli(Some(console)).await?;
    }

    let mut client = connect_target(target).await?;
    let lifecycle = client.lifecycle().await.map_err(|error| {
        CliError::IpcError(format!("Failed to read playitd lifecycle: {error}"))
    })?;

    match lifecycle {
        AgentLifecycle::WaitingForSecret => Ok(()),
        AgentLifecycle::HasInvalidSecret(error) => Err(CliError::ServiceError(format!(
            "playitd is not waiting for setup because it has an invalid secret configuration: {}. Reset the daemon secret first.",
            error.message
        ))),
        AgentLifecycle::Starting => Err(CliError::ServiceError(
            "playitd is starting and is not waiting for setup".to_string(),
        )),
        AgentLifecycle::Running(_) => Err(CliError::ServiceError(
            "playitd already has a configured secret and is not waiting for setup. Run `playit reset` before claiming a new agent."
                .to_string(),
        )),
        AgentLifecycle::Stopping => Err(CliError::ServiceError(
            "playitd is stopping and is not waiting for setup".to_string(),
        )),
        AgentLifecycle::Error(error) => Err(CliError::ServiceError(format!(
            "playitd reported an error and is not waiting for setup: {}",
            error.message
        ))),
    }
}

pub async fn provision_service_secret(
    console: &mut ConsoleUi,
    target: &CliTarget,
    secret: &str,
) -> Result<(), CliError> {
    if matches!(target, CliTarget::InstalledService) {
        ensure_installed_service_running_for_cli(Some(console)).await?;
    }

    let mut client = connect_target(target).await?;
    let response = client
        .set_secret(secret)
        .await
        .map_err(|error| CliError::IpcError(format!("Failed to provision secret: {error}")))?;

    if !response.accepted {
        return Err(CliError::IpcError(
            response
                .message
                .unwrap_or_else(|| "playitd rejected the secret".to_string()),
        ));
    }

    Ok(())
}

pub async fn run_reset_command(target: &CliTarget) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let reset_response = client
        .reset_secret()
        .await
        .map_err(|error| CliError::IpcError(format!("Failed to reset secret: {error}")))?;

    if !reset_response.accepted {
        return Err(CliError::IpcError(reset_response.message.unwrap_or_else(
            || "playitd rejected the reset request".to_string(),
        )));
    }

    let stop_response = client.stop().await.map_err(|error| {
        CliError::IpcError(format!(
            "Secret was reset, but failed to stop playitd: {error}"
        ))
    })?;

    if !stop_response.accepted {
        return Err(CliError::IpcError(stop_response.message.unwrap_or_else(
            || "Secret was reset, but playitd rejected the stop request".to_string(),
        )));
    }

    let reset_message = reset_response
        .message
        .unwrap_or_else(|| "playitd reset the secret file".to_string());
    let stop_message = stop_response
        .message
        .unwrap_or_else(|| "shutdown requested".to_string());

    println!("{reset_message}");
    println!("playitd stop requested: {stop_message}");
    Ok(())
}

pub async fn run_secret_path_command(target: &CliTarget) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let response = client
        .get_secret_path()
        .await
        .map_err(|error| CliError::IpcError(format!("Failed to read secret path: {error}")))?;

    let Some(secret_path) = response.secret_path else {
        return Err(CliError::IpcError(
            "playitd is using an inline --secret, so no secret file path is available".to_string(),
        ));
    };

    println!("{secret_path}");
    Ok(())
}

pub async fn run_account_login_url_command(target: &CliTarget) -> Result<(), CliError> {
    let mut client = connect_target(target).await?;
    let response = client.get_account_login_url().await.map_err(|error| {
        CliError::IpcError(format!("Failed to create account login URL: {error}"))
    })?;

    println!("{}", response.login_url);
    Ok(())
}

async fn connect_target(target: &CliTarget) -> Result<IpcClient, CliError> {
    IpcClient::connect_with_path(target.socket_path())
        .await
        .map_err(|_| ipc_connection_error())
}

async fn ensure_installed_service_running_for_cli(
    console: Option<&mut ConsoleUi>,
) -> Result<InstalledServiceStartState, CliError> {
    if IpcClient::is_running(get_default_socket_path()).await {
        return Ok(InstalledServiceStartState::AlreadyRunning);
    }

    #[cfg(target_os = "linux")]
    {
        if is_systemd_service_active().map_err(|error| {
            CliError::ServiceError(format!("Failed to check service status: {error}"))
        })? {
            for _ in 0..20 {
                if IpcClient::is_running(get_default_socket_path()).await {
                    return Ok(InstalledServiceStartState::AlreadyRunning);
                }

                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            return Err(CliError::ServiceError(
                linux_installed_service_unreachable_message(),
            ));
        }

        if let Some(console) = console {
            let should_start = console
                .yn_question(
                    linux_service_start_prompt(current_user_is_root()),
                    Some(true),
                )
                .await?;

            if !should_start {
                return Err(CliError::ServiceError(
                    "The playit service is not running. Start it with `systemctl start playit` and try again."
                        .to_string(),
                ));
            }
        }
    }

    ensure_installed_service_running()
        .await
        .map_err(|error| CliError::ServiceError(format!("Failed to start service: {error}")))?;

    Ok(InstalledServiceStartState::Started)
}

#[cfg(target_os = "linux")]
fn linux_service_start_prompt(is_root: bool) -> String {
    let mut prompt = String::from(
        "The playit service is not running.\nWould you like us to start it?\n\nCommand: systemctl start playit",
    );

    if !is_root {
        prompt.push_str("\nThis will ask you for your password.");
    }

    prompt
}

#[cfg(target_os = "linux")]
fn current_user_is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(target_os = "linux")]
fn linux_installed_service_unreachable_message() -> String {
    let socket_path = get_default_socket_path();

    match linux_socket_access_diagnostic(socket_path) {
        Some(message) => message,
        None => format!(
            "The playit service is running, but its IPC socket at {socket_path} is still not reachable."
        ),
    }
}

#[cfg(target_os = "linux")]
fn linux_socket_access_diagnostic(socket_path: &str) -> Option<String> {
    let path = Path::new(socket_path);
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(format!(
                "The playit service is running, but its IPC socket at {socket_path} does not exist."
            ));
        }
        Err(error) => {
            return Some(format!(
                "The playit service is running, but the IPC socket at {socket_path} could not be inspected: {error}"
            ));
        }
    };

    if !metadata.file_type().is_socket() {
        return Some(format!(
            "The playit service is running, but {socket_path} exists and is not a Unix socket."
        ));
    }

    let current_uid = unsafe { libc::geteuid() };
    let current_gid = unsafe { libc::getegid() };
    let socket_uid = metadata.uid();
    let socket_gid = metadata.gid();
    let socket_mode = metadata.mode() & 0o777;
    let socket_group_name = lookup_group_name(socket_gid);

    if current_user_can_write_socket(&metadata) {
        return None;
    }

    if socket_group_name.as_deref() == Some("playit") {
        return Some(format!(
            "The playit service is running, but its IPC socket at {socket_path} is restricted to the `playit` group. Add this user to that group with `usermod -aG playit <username>` and start a new login session. Current user uid={current_uid}, gid={current_gid}; socket owner uid={socket_uid}, gid={socket_gid}, mode={socket_mode:o}."
        ));
    }

    Some(format!(
        "The playit service is running, but the current user cannot access its IPC socket at {socket_path}. Current user uid={current_uid}, gid={current_gid}; socket owner uid={socket_uid}, gid={socket_gid}, mode={socket_mode:o}."
    ))
}

#[cfg(target_os = "linux")]
fn current_user_can_write_socket(metadata: &fs::Metadata) -> bool {
    let mode = metadata.mode();
    let uid = metadata.uid();
    let gid = metadata.gid();
    let current_uid = unsafe { libc::geteuid() };

    if current_uid == 0 {
        return true;
    }

    if current_uid == uid {
        return mode & 0o200 != 0;
    }

    if current_user_in_group(gid) {
        return mode & 0o020 != 0;
    }

    mode & 0o002 != 0
}

#[cfg(target_os = "linux")]
fn current_user_in_group(target_gid: u32) -> bool {
    if unsafe { libc::getegid() } == target_gid {
        return true;
    }

    let group_count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if group_count <= 0 {
        return false;
    }

    let mut groups = vec![0 as libc::gid_t; group_count as usize];
    let loaded = unsafe { libc::getgroups(group_count, groups.as_mut_ptr()) };
    if loaded <= 0 {
        return false;
    }

    groups
        .into_iter()
        .take(loaded as usize)
        .any(|group| group == target_gid)
}

#[cfg(target_os = "linux")]
fn lookup_group_name(group_gid: u32) -> Option<String> {
    let mut group = std::mem::MaybeUninit::<libc::group>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buf_len = 1024usize;

    loop {
        let mut buf = vec![0u8; buf_len];
        let status = unsafe {
            libc::getgrgid_r(
                group_gid,
                group.as_mut_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut result,
            )
        };

        if status == 0 {
            if result.is_null() {
                return None;
            }

            let group = unsafe { group.assume_init() };
            let name = unsafe { CStr::from_ptr(group.gr_name) };
            return Some(name.to_string_lossy().into_owned());
        }

        if status == libc::ERANGE {
            buf_len *= 2;
            continue;
        }

        return None;
    }
}

fn ipc_connection_error() -> CliError {
    CliError::IpcError(
        "Failed to connect to playitd over IPC. Start playitd and try again.".to_string(),
    )
}

fn initial_attach_error(target: &CliTarget, error_context: &AttachErrorContext) -> CliError {
    match error_context {
        AttachErrorContext::Standard => ipc_connection_error(),
        AttachErrorContext::AutoCommand {
            start_attempt_failed,
        } => auto_attach_error(target, start_attempt_failed.as_deref()),
    }
}

fn auto_attach_error(target: &CliTarget, start_attempt_failed: Option<&str>) -> CliError {
    match target {
        CliTarget::InstalledService => match start_attempt_failed {
            Some(error) if error.starts_with("The playit service is running, but") => {
                CliError::IpcError(error.to_string())
            }
            Some(error) => CliError::IpcError(format!(
                "Failed to connect to playitd over IPC. playit-cli auto mode also tried starting playitd first, but that start attempt failed: {error}"
            )),
            None => CliError::IpcError(
                "Failed to connect to playitd over IPC. playit-cli auto mode prepared the service first, but it is still not reachable."
                    .to_string(),
            ),
        },
        CliTarget::ExplicitSocket(_) => ipc_connection_error(),
    }
}

fn attach_lost_message(target: &CliTarget, error: &str) -> String {
    match target {
        CliTarget::InstalledService => {
            format!("Connection to playitd lost: {error}. Run \"playit attach\" to reconnect.")
        }
        CliTarget::ExplicitSocket(path) => format!(
            "Connection to playitd lost: {error}. Reattach with \"playit attach --socket-path {}\" once the daemon is reachable again.",
            path
        ),
    }
}

fn format_timestamp_millis(millis: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(millis as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
        .unwrap_or_else(|| format!("{millis}ms"))
}

fn format_service_phase(phase: &ServicePhase) -> &'static str {
    match phase {
        ServicePhase::WaitingForSecret => "waiting_for_secret",
        ServicePhase::HasInvalidSecret => "has_invalid_secret",
        ServicePhase::Starting => "starting",
        ServicePhase::Running => "running",
        ServicePhase::Stopping => "stopping",
        ServicePhase::Error => "error",
    }
}

fn format_log_level(level: &ServiceLogLevel) -> &'static str {
    match level {
        ServiceLogLevel::Trace => "TRACE",
        ServiceLogLevel::Debug => "DEBUG",
        ServiceLogLevel::Info => "INFO",
        ServiceLogLevel::Warn => "WARN",
        ServiceLogLevel::Error => "ERROR",
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{
        CliTarget, auto_attach_error, linux_service_start_prompt, linux_socket_access_diagnostic,
    };

    #[test]
    fn linux_start_prompt_includes_command() {
        let prompt = linux_service_start_prompt(true);
        assert!(prompt.contains("The playit service is not running."));
        assert!(prompt.contains("Command: systemctl start playit"));
        assert!(!prompt.contains("password"));
    }

    #[test]
    fn linux_start_prompt_warns_non_root_about_password() {
        let prompt = linux_service_start_prompt(false);
        assert!(prompt.contains("This will ask you for your password."));
    }

    #[test]
    fn auto_attach_error_surfaces_precise_linux_socket_message() {
        let error = auto_attach_error(
            &CliTarget::InstalledService,
            Some(
                "The playit service is running, but the current user cannot access its IPC socket at /var/run/playitd.sock.",
            ),
        );

        assert_eq!(
            error.to_string(),
            "The playit service is running, but the current user cannot access its IPC socket at /var/run/playitd.sock."
        );
    }

    #[test]
    fn auto_attach_error_surfaces_playit_group_message() {
        let error = auto_attach_error(
            &CliTarget::InstalledService,
            Some(
                "The playit service is running, but its IPC socket at /var/run/playitd.sock is restricted to the `playit` group.",
            ),
        );

        assert_eq!(
            error.to_string(),
            "The playit service is running, but its IPC socket at /var/run/playitd.sock is restricted to the `playit` group."
        );
    }

    #[test]
    fn linux_socket_diagnostic_reports_missing_socket() {
        let missing = linux_socket_access_diagnostic("/tmp/playit-socket-that-does-not-exist");
        assert!(
            missing
                .expect("missing socket should produce a diagnostic")
                .contains("does not exist")
        );
    }
}
