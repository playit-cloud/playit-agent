use std::time::Duration;

use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playit_platform::linux::{SocketAccessIssue, inspect_socket_access};
use playit_platform::service::{ServiceManagerKind, ServiceState, installed_service_state};
use playit_platform::unix::current_user_is_root;

use crate::{CliError, ui::ConsoleUi};

pub(crate) async fn prepare_installed_service_for_cli(
    console: Option<&mut ConsoleUi>,
    service_manager: ServiceManagerKind,
) -> Result<bool, CliError> {
    if installed_service_is_active(service_manager)? {
        for _ in 0..20 {
            if IpcClient::is_running(get_default_socket_path()).await {
                return Ok(true);
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        return Err(CliError::ServiceError(
            installed_service_unreachable_message(service_manager),
        ));
    }

    if let Some(console) = console {
        let should_start = console
            .yn_question(
                service_start_prompt(service_manager, current_user_is_root()),
                Some(true),
            )
            .await?;

        if !should_start {
            return Err(CliError::ServiceError(format!(
                "The playit service is not running. Start it with `{}`, then run `playit` again.",
                service_start_command(service_manager, false)
            )));
        }
    }

    Ok(false)
}

pub(crate) fn installed_service_is_active(
    service_manager: ServiceManagerKind,
) -> Result<bool, CliError> {
    installed_service_state(service_manager)
        .map(|state| state == ServiceState::Running)
        .map_err(|error| CliError::ServiceError(format!("Failed to check service status: {error}")))
}

pub(crate) fn is_linux_socket_access_message(message: &str) -> bool {
    message.starts_with("The playit service is running, but")
}

fn service_start_prompt(service_manager: ServiceManagerKind, is_root: bool) -> String {
    let command = service_start_command(service_manager, is_root);
    let mut prompt = format!(
        "The playit service is not running.\nStart it now so playit can run in the background?\n\nCommand: {command}",
    );

    if !is_root {
        prompt.push_str("\nYou may be asked for your password.");
    }

    prompt
}

fn service_start_command(service_manager: ServiceManagerKind, is_root: bool) -> &'static str {
    match (service_manager, is_root) {
        (ServiceManagerKind::Systemd, true) => "systemctl start playit",
        (ServiceManagerKind::Systemd, false) => "sudo systemctl start playit",
        (ServiceManagerKind::OpenRc, true) => "rc-service playit start",
        (ServiceManagerKind::OpenRc, false) => "sudo rc-service playit start",
        _ => unreachable!("Linux CLI only selects systemd or OpenRC"),
    }
}

fn installed_service_unreachable_message(service_manager: ServiceManagerKind) -> String {
    let socket_path = get_default_socket_path();

    match inspect_socket_access(socket_path) {
        Some(issue) => format_socket_access_issue(socket_path, &issue, service_manager),
        None => format!(
            "The playit service is running, but this shell cannot reach its IPC socket:\n  {socket_path}\n\nTry running `playit status` again in a few seconds. If it still fails, restart the service with:\n  {}",
            service_restart_command(service_manager)
        ),
    }
}

fn format_socket_access_issue(
    socket_path: &str,
    issue: &SocketAccessIssue,
    service_manager: ServiceManagerKind,
) -> String {
    match issue {
        SocketAccessIssue::MissingSocket => {
            format!(
                "The playit service is running, but its IPC socket does not exist yet:\n  {socket_path}\n\nRestart the service, then try again:\n  {}",
                service_restart_command(service_manager)
            )
        }
        SocketAccessIssue::InspectFailed(error) => {
            format!(
                "The playit service is running, but playit could not inspect its IPC socket:\n  {socket_path}\n\nError: {error}"
            )
        }
        SocketAccessIssue::NotASocket => {
            format!(
                "The playit service is running, but this path is not a Unix socket:\n  {socket_path}\n\nRemove or rename that file, then restart the service:\n  {}",
                service_restart_command(service_manager)
            )
        }
        SocketAccessIssue::GroupJoinRequired => format_playit_group_join_message(socket_path),
        SocketAccessIssue::GroupRefreshRequired => format_playit_group_refresh_message(socket_path),
        SocketAccessIssue::PermissionDenied {
            current_uid,
            current_gid,
            socket_uid,
            socket_gid,
            socket_mode,
        } => format!(
            "The playit service is running, but this user cannot access its IPC socket:\n  {socket_path}\n\nCurrent user uid={current_uid}, gid={current_gid}\nSocket owner uid={socket_uid}, gid={socket_gid}, mode={socket_mode:o}\n\nCheck the socket permissions or run playit from a user that can access this socket."
        ),
    }
}

fn service_restart_command(service_manager: ServiceManagerKind) -> &'static str {
    match service_manager {
        ServiceManagerKind::Systemd => "sudo systemctl restart playit",
        ServiceManagerKind::OpenRc => "sudo rc-service playit restart",
        _ => unreachable!("Linux CLI only selects systemd or OpenRC"),
    }
}

fn format_playit_group_join_message(socket_path: &str) -> String {
    format!(
        "The playit service is running, but this shell cannot access its IPC socket:\n  {socket_path}\n\nThe socket is restricted to the `playit` group. Add your user to that group:\n  sudo usermod -aG playit $USER\n\nThen refresh group membership for this shell:\n  newgrp playit\n\nAfter that, run:\n  playit"
    )
}

fn format_playit_group_refresh_message(socket_path: &str) -> String {
    format!(
        "The playit service is running, but this shell cannot access its IPC socket:\n  {socket_path}\n\nYour user is already in the `playit` group, but this shell has not picked up that membership yet.\n\nRefresh group membership for this shell:\n  newgrp playit\n\nThen run:\n  playit"
    )
}
