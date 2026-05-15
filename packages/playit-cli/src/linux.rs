use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    time::Duration,
};

use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playitd::manager::is_systemd_service_active;
use playitd::unix_account::{
    current_process_has_group, current_user_account, current_user_is_root, effective_gid,
    effective_uid, group_info_by_gid,
};

use crate::{CliError, ui::ConsoleUi};

const PLAYIT_GROUP_NAME: &str = "playit";

pub(crate) async fn prepare_installed_service_for_cli(
    console: Option<&mut ConsoleUi>,
) -> Result<bool, CliError> {
    if installed_service_is_active()? {
        for _ in 0..20 {
            if IpcClient::is_running(get_default_socket_path()).await {
                return Ok(true);
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        return Err(CliError::ServiceError(
            installed_service_unreachable_message(),
        ));
    }

    if let Some(console) = console {
        let should_start = console
            .yn_question(service_start_prompt(current_user_is_root()), Some(true))
            .await?;

        if !should_start {
            return Err(CliError::ServiceError(
                "The playit service is not running. Start it with `sudo systemctl start playit`, then run `playit` again."
                    .to_string(),
            ));
        }
    }

    Ok(false)
}

pub(crate) fn installed_service_is_active() -> Result<bool, CliError> {
    is_systemd_service_active()
        .map_err(|error| CliError::ServiceError(format!("Failed to check service status: {error}")))
}

pub(crate) fn is_linux_socket_access_message(message: &str) -> bool {
    message.starts_with("The playit service is running, but")
}

fn service_start_prompt(is_root: bool) -> String {
    let command = if is_root {
        "systemctl start playit"
    } else {
        "sudo systemctl start playit"
    };
    let mut prompt = format!(
        "The playit service is not running.\nStart it now so playit can run in the background?\n\nCommand: {command}",
    );

    if !is_root {
        prompt.push_str("\nYou may be asked for your password.");
    }

    prompt
}

fn installed_service_unreachable_message() -> String {
    let socket_path = get_default_socket_path();

    match socket_access_issue(socket_path) {
        Some(issue) => format_socket_access_issue(socket_path, &issue),
        None => format!(
            "The playit service is running, but this shell cannot reach its IPC socket:\n  {socket_path}\n\nTry running `playit status` again in a few seconds. If it still fails, restart the service with:\n  sudo systemctl restart playit"
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LinuxSocketAccessIssue {
    MissingSocket,
    InspectFailed(String),
    NotASocket,
    PlayitGroupJoinRequired,
    PlayitGroupRefreshRequired,
    GenericPermissionDenied {
        current_uid: u32,
        current_gid: u32,
        socket_uid: u32,
        socket_gid: u32,
        socket_mode: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinuxGroupInfo {
    name: String,
    members: Vec<String>,
}

fn socket_access_issue(socket_path: &str) -> Option<LinuxSocketAccessIssue> {
    let path = Path::new(socket_path);
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(LinuxSocketAccessIssue::MissingSocket);
        }
        Err(error) => return Some(LinuxSocketAccessIssue::InspectFailed(error.to_string())),
    };

    if !metadata.file_type().is_socket() {
        return Some(LinuxSocketAccessIssue::NotASocket);
    }

    let current_uid = effective_uid();
    let current_gid = effective_gid();
    let socket_uid = metadata.uid();
    let socket_gid = metadata.gid();
    let socket_mode = metadata.mode() & 0o777;
    let socket_group = lookup_group_info(socket_gid);
    let socket_group_name = socket_group.as_ref().map(|group| group.name.as_str());

    if current_user_can_write_socket(&metadata) {
        return None;
    }

    if socket_group_name == Some(PLAYIT_GROUP_NAME) {
        match current_user_account_is_configured_for_group(socket_gid, socket_group.as_ref()) {
            Some(true) => return Some(LinuxSocketAccessIssue::PlayitGroupRefreshRequired),
            Some(false) => return Some(LinuxSocketAccessIssue::PlayitGroupJoinRequired),
            None => {}
        }
    }

    Some(LinuxSocketAccessIssue::GenericPermissionDenied {
        current_uid,
        current_gid,
        socket_uid,
        socket_gid,
        socket_mode,
    })
}

fn format_socket_access_issue(socket_path: &str, issue: &LinuxSocketAccessIssue) -> String {
    match issue {
        LinuxSocketAccessIssue::MissingSocket => {
            format!(
                "The playit service is running, but its IPC socket does not exist yet:\n  {socket_path}\n\nRestart the service, then try again:\n  sudo systemctl restart playit"
            )
        }
        LinuxSocketAccessIssue::InspectFailed(error) => {
            format!(
                "The playit service is running, but playit could not inspect its IPC socket:\n  {socket_path}\n\nError: {error}"
            )
        }
        LinuxSocketAccessIssue::NotASocket => {
            format!(
                "The playit service is running, but this path is not a Unix socket:\n  {socket_path}\n\nRemove or rename that file, then restart the service:\n  sudo systemctl restart playit"
            )
        }
        LinuxSocketAccessIssue::PlayitGroupJoinRequired => {
            format_playit_group_join_message(socket_path)
        }
        LinuxSocketAccessIssue::PlayitGroupRefreshRequired => {
            format_playit_group_refresh_message(socket_path)
        }
        LinuxSocketAccessIssue::GenericPermissionDenied {
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

fn current_user_account_is_configured_for_group(
    target_gid: u32,
    group: Option<&LinuxGroupInfo>,
) -> Option<bool> {
    let account = current_user_account()?;

    if account.primary_gid == target_gid {
        return Some(true);
    }

    let group = group?;
    Some(
        group
            .members
            .iter()
            .any(|member| member == &account.username),
    )
}

fn current_user_can_write_socket(metadata: &fs::Metadata) -> bool {
    let mode = metadata.mode();
    let uid = metadata.uid();
    let gid = metadata.gid();
    let current_uid = effective_uid();

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

fn current_user_in_group(target_gid: u32) -> bool {
    current_process_has_group(target_gid)
}

fn lookup_group_info(group_gid: u32) -> Option<LinuxGroupInfo> {
    group_info_by_gid(group_gid).map(|group| LinuxGroupInfo {
        name: group.name,
        members: group.members,
    })
}
