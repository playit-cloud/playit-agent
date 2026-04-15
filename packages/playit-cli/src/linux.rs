use std::{
    ffi::CStr,
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    time::Duration,
};

use playit_ipc::ipc::{IpcClient, get_default_socket_path};
use playitd::manager::is_systemd_service_active;

use crate::{CliError, ui::ConsoleUi};

const PLAYIT_GROUP_NAME: &str = "playit";

pub(crate) async fn prepare_installed_service_for_cli(
    console: Option<&mut ConsoleUi>,
) -> Result<bool, CliError> {
    if is_systemd_service_active().map_err(|error| {
        CliError::ServiceError(format!("Failed to check service status: {error}"))
    })? {
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
                "The playit service is not running. Start it with `systemctl start playit` and try again."
                    .to_string(),
            ));
        }
    }

    Ok(false)
}

pub(crate) fn is_linux_socket_access_message(message: &str) -> bool {
    message.starts_with("The playit service is running, but")
}

fn service_start_prompt(is_root: bool) -> String {
    let mut prompt = String::from(
        "The playit service is not running.\nWould you like us to start it?\n\nCommand: systemctl start playit",
    );

    if !is_root {
        prompt.push_str("\nThis will ask you for your password.");
    }

    prompt
}

fn current_user_is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn installed_service_unreachable_message() -> String {
    let socket_path = get_default_socket_path();

    match socket_access_issue(socket_path) {
        Some(issue) => format_socket_access_issue(socket_path, &issue),
        None => format!(
            "The playit service is running, but its IPC socket at {socket_path} is still not reachable."
        ),
    }
}

fn socket_access_diagnostic(socket_path: &str) -> Option<String> {
    socket_access_issue(socket_path).map(|issue| format_socket_access_issue(socket_path, &issue))
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
struct LinuxUserAccount {
    username: String,
    primary_gid: u32,
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

    let current_uid = unsafe { libc::geteuid() as u32 };
    let current_gid = unsafe { libc::getegid() as u32 };
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
                "The playit service is running, but its IPC socket at {socket_path} does not exist."
            )
        }
        LinuxSocketAccessIssue::InspectFailed(error) => {
            format!(
                "The playit service is running, but the IPC socket at {socket_path} could not be inspected: {error}"
            )
        }
        LinuxSocketAccessIssue::NotASocket => {
            format!(
                "The playit service is running, but {socket_path} exists and is not a Unix socket."
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
            "The playit service is running, but the current user cannot access its IPC socket:\n  {socket_path}\n\nCurrent user uid={current_uid}, gid={current_gid}\nSocket owner uid={socket_uid}, gid={socket_gid}, mode={socket_mode:o}"
        ),
    }
}

fn format_playit_group_join_message(socket_path: &str) -> String {
    format!(
        "The playit service is running, but this shell cannot access its IPC socket:\n  {socket_path}\n\nThe socket is restricted to the `playit` group.\n\nAdd the current user to that group:\n  sudo usermod -aG playit $USER\n\nThen refresh group membership in this shell:\n  newgrp playit\n\nAfter that, run:\n  playit"
    )
}

fn format_playit_group_refresh_message(socket_path: &str) -> String {
    format!(
        "The playit service is running, but this shell cannot access its IPC socket:\n  {socket_path}\n\nThis user is already configured for the `playit` group, but the current shell has not picked up that group yet.\n\nRefresh group membership in this shell:\n  newgrp playit\n\nThen run:\n  playit"
    )
}

fn current_user_account_is_configured_for_group(
    target_gid: u32,
    group: Option<&LinuxGroupInfo>,
) -> Option<bool> {
    let account = lookup_current_user_account()?;

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

fn lookup_current_user_account() -> Option<LinuxUserAccount> {
    let mut passwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buf_len = 1024usize;
    let current_uid = unsafe { libc::geteuid() };

    loop {
        let mut buf = vec![0u8; buf_len];
        let status = unsafe {
            libc::getpwuid_r(
                current_uid,
                passwd.as_mut_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut result,
            )
        };

        if status == 0 {
            if result.is_null() {
                return None;
            }

            let passwd = unsafe { passwd.assume_init() };
            let username = unsafe { CStr::from_ptr(passwd.pw_name) }
                .to_string_lossy()
                .into_owned();

            return Some(LinuxUserAccount {
                username,
                primary_gid: passwd.pw_gid,
            });
        }

        if status == libc::ERANGE {
            buf_len *= 2;
            continue;
        }

        return None;
    }
}

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

fn lookup_group_info(group_gid: u32) -> Option<LinuxGroupInfo> {
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
            let name = unsafe { CStr::from_ptr(group.gr_name) }
                .to_string_lossy()
                .into_owned();
            let mut members = Vec::new();
            let mut member_ptr = group.gr_mem;

            while !member_ptr.is_null() {
                let member = unsafe { *member_ptr };
                if member.is_null() {
                    break;
                }

                members.push(
                    unsafe { CStr::from_ptr(member) }
                        .to_string_lossy()
                        .into_owned(),
                );
                member_ptr = unsafe { member_ptr.add(1) };
            }

            return Some(LinuxGroupInfo { name, members });
        }

        if status == libc::ERANGE {
            buf_len *= 2;
            continue;
        }

        return None;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LinuxSocketAccessIssue, format_playit_group_join_message,
        format_playit_group_refresh_message, format_socket_access_issue,
        is_linux_socket_access_message, service_start_prompt, socket_access_diagnostic,
    };
    use crate::{client::CliTarget, client::auto_attach_error};

    #[test]
    fn linux_start_prompt_includes_command() {
        let prompt = service_start_prompt(true);
        assert!(prompt.contains("The playit service is not running."));
        assert!(prompt.contains("Command: systemctl start playit"));
        assert!(!prompt.contains("password"));
    }

    #[test]
    fn linux_start_prompt_warns_non_root_about_password() {
        let prompt = service_start_prompt(false);
        assert!(prompt.contains("This will ask you for your password."));
    }

    #[test]
    fn linux_socket_access_message_detector_matches_linux_message() {
        assert!(is_linux_socket_access_message(
            "The playit service is running, but its IPC socket at /var/run/playitd.sock does not exist."
        ));
    }

    #[test]
    fn auto_attach_error_surfaces_playit_group_message() {
        let error = auto_attach_error(
            &CliTarget::InstalledService,
            Some(&format_playit_group_join_message("/var/run/playitd.sock")),
        );

        assert!(error.to_string().contains("sudo usermod -aG playit $USER"));
        assert!(error.to_string().contains("newgrp playit"));
    }

    #[test]
    fn linux_socket_diagnostic_reports_missing_socket() {
        let missing = socket_access_diagnostic("/tmp/playit-socket-that-does-not-exist");
        assert!(
            missing
                .expect("missing socket should produce a diagnostic")
                .contains("does not exist")
        );
    }

    #[test]
    fn playit_group_join_message_includes_copy_paste_commands() {
        let message = format_playit_group_join_message("/var/run/playitd.sock");
        assert!(message.contains("this shell cannot access its IPC socket"));
        assert!(message.contains("sudo usermod -aG playit $USER"));
        assert!(message.contains("newgrp playit"));
        assert!(message.contains("\n  /var/run/playitd.sock\n"));
    }

    #[test]
    fn playit_group_refresh_message_only_includes_newgrp() {
        let message = format_playit_group_refresh_message("/var/run/playitd.sock");
        assert!(message.contains("already configured for the `playit` group"));
        assert!(message.contains("newgrp playit"));
        assert!(!message.contains("sudo usermod -aG playit"));
    }

    #[test]
    fn generic_permission_message_is_multiline() {
        let message = format_socket_access_issue(
            "/var/run/playitd.sock",
            &LinuxSocketAccessIssue::GenericPermissionDenied {
                current_uid: 1000,
                current_gid: 1000,
                socket_uid: 0,
                socket_gid: 980,
                socket_mode: 0o660,
            },
        );

        assert!(
            message.contains(
                "The playit service is running, but the current user cannot access its IPC socket:\n  /var/run/playitd.sock"
            )
        );
        assert!(message.contains("Current user uid=1000, gid=1000"));
        assert!(message.contains("Socket owner uid=0, gid=980, mode=660"));
    }
}
