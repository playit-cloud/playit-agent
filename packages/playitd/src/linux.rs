use std::{
    ffi::CString,
    io,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::Path,
    process::Command,
};

use playit_ipc::ipc::IpcError;

use crate::manager::ServiceManagerError;

const SYSTEMD_SERVICE_NAME: &str = "playit";
const OPENRC_SERVICE_NAME: &str = "playit";
const PLAYIT_SOCKET_GROUP_NAME: &str = "playit";
const PLAYIT_SOCKET_MODE: u32 = 0o660;

pub(crate) fn start_systemd_service() -> Result<(), ServiceManagerError> {
    run_systemctl(&systemd_start_args(), ServiceManagerError::StartFailed)
}

pub(crate) fn start_openrc_service() -> Result<(), ServiceManagerError> {
    run_rc_service(&openrc_start_args(), ServiceManagerError::StartFailed)
}

pub(crate) fn is_systemd_service_active() -> Result<bool, ServiceManagerError> {
    let args = systemd_is_active_args();
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| ServiceManagerError::NotAvailable(format!("Failed to run systemctl: {e}")))?;

    match output.status.code() {
        Some(0) => Ok(true),
        Some(3) | Some(4) => Ok(false),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!("exit status {}", output.status)
            };

            Err(ServiceManagerError::NotAvailable(format!(
                "systemctl {} failed: {}",
                args.join(" "),
                detail
            )))
        }
    }
}

pub(crate) fn is_openrc_service_active() -> Result<bool, ServiceManagerError> {
    let args = openrc_status_args();
    let output = Command::new("rc-service")
        .args(args)
        .output()
        .map_err(|e| ServiceManagerError::NotAvailable(format!("Failed to run rc-service: {e}")))?;

    Ok(output.status.success())
}

pub(crate) fn stop_systemd_service() -> Result<(), ServiceManagerError> {
    run_systemctl(&systemd_stop_args(), ServiceManagerError::StopFailed)
}

pub(crate) fn stop_openrc_service() -> Result<(), ServiceManagerError> {
    run_rc_service(&openrc_stop_args(), ServiceManagerError::StopFailed)
}

pub(crate) fn configure_socket_permissions(socket_path: &str) -> Result<(), IpcError> {
    let Some(target) = socket_permission_target(socket_path, crate::unix_account::effective_uid())
    else {
        return Ok(());
    };

    if !Path::new(target.path).exists() {
        return Err(IpcError::BindFailed(io::Error::new(
            io::ErrorKind::NotFound,
            format!("IPC socket {} was not created", target.path),
        )));
    }

    let group_gid = if target.chown_group {
        match crate::unix_account::group_gid_by_name(target.group_name) {
            Some(group_gid) => Some(group_gid),
            None => {
                tracing::warn!(
                    group = target.group_name,
                    socket_path = %target.path,
                    "IPC socket group is missing, leaving default socket permissions in place"
                );
                None
            }
        }
    } else {
        None
    };

    apply_socket_permissions(target.path, group_gid, target.mode)
}

fn systemd_start_args() -> [&'static str; 2] {
    ["start", SYSTEMD_SERVICE_NAME]
}

fn systemd_is_active_args() -> [&'static str; 3] {
    ["is-active", "--quiet", SYSTEMD_SERVICE_NAME]
}

fn systemd_stop_args() -> [&'static str; 2] {
    ["stop", SYSTEMD_SERVICE_NAME]
}

fn openrc_start_args() -> [&'static str; 2] {
    [OPENRC_SERVICE_NAME, "start"]
}

fn openrc_status_args() -> [&'static str; 2] {
    [OPENRC_SERVICE_NAME, "status"]
}

fn openrc_stop_args() -> [&'static str; 2] {
    [OPENRC_SERVICE_NAME, "stop"]
}

fn run_systemctl(
    args: &[&str],
    err_builder: fn(String) -> ServiceManagerError,
) -> Result<(), ServiceManagerError> {
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

fn run_rc_service(
    args: &[&str],
    err_builder: fn(String) -> ServiceManagerError,
) -> Result<(), ServiceManagerError> {
    let output = Command::new("rc-service")
        .args(args)
        .output()
        .map_err(|e| err_builder(format!("Failed to run rc-service: {e}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };

    Err(err_builder(format!(
        "rc-service {} failed: {}",
        args.join(" "),
        detail
    )))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinuxSocketPermissionTarget<'a> {
    path: &'a str,
    group_name: &'static str,
    mode: u32,
    chown_group: bool,
}

fn socket_permission_target(
    socket_path: &str,
    effective_uid: u32,
) -> Option<LinuxSocketPermissionTarget<'_>> {
    if socket_path.starts_with('@') || socket_path.starts_with(r"\\.\pipe\") {
        return None;
    }

    Some(LinuxSocketPermissionTarget {
        path: socket_path,
        group_name: PLAYIT_SOCKET_GROUP_NAME,
        mode: PLAYIT_SOCKET_MODE,
        chown_group: effective_uid == 0,
    })
}

fn apply_socket_permissions(
    socket_path: &str,
    group_gid: Option<u32>,
    mode: u32,
) -> Result<(), IpcError> {
    let path = Path::new(socket_path);
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|e| {
        IpcError::BindFailed(io::Error::new(
            e.kind(),
            format!("failed to chmod IPC socket {socket_path} to {mode:o}: {e}"),
        ))
    })?;

    let Some(group_gid) = group_gid else {
        return Ok(());
    };

    let path_cstr = CString::new(path.as_os_str().as_bytes()).map_err(|e| {
        IpcError::BindFailed(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid IPC socket path {socket_path:?}: {e}"),
        ))
    })?;

    let chown_status = unsafe { libc::chown(path_cstr.as_ptr(), u32::MAX, group_gid) };
    if chown_status != 0 {
        return Err(IpcError::BindFailed(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "failed to chown IPC socket {socket_path} to group gid {group_gid}: {}",
                io::Error::last_os_error()
            ),
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        PLAYIT_SOCKET_GROUP_NAME, PLAYIT_SOCKET_MODE, openrc_start_args, openrc_status_args,
        openrc_stop_args, socket_permission_target, systemd_start_args, systemd_stop_args,
    };

    #[test]
    fn systemd_command_args_are_stable() {
        assert_eq!(systemd_start_args(), ["start", "playit"]);
        assert_eq!(systemd_stop_args(), ["stop", "playit"]);
    }

    #[test]
    fn openrc_command_args_are_stable() {
        assert_eq!(openrc_start_args(), ["playit", "start"]);
        assert_eq!(openrc_status_args(), ["playit", "status"]);
        assert_eq!(openrc_stop_args(), ["playit", "stop"]);
    }

    #[test]
    fn root_socket_target_chmods_and_chowns_group() {
        let target = socket_permission_target("/run/playit/playitd.sock", 0).unwrap();

        assert_eq!(target.path, "/run/playit/playitd.sock");
        assert_eq!(target.group_name, PLAYIT_SOCKET_GROUP_NAME);
        assert_eq!(target.mode, PLAYIT_SOCKET_MODE);
        assert!(target.chown_group);
    }

    #[test]
    fn non_root_socket_target_chmods_without_chown() {
        let target = socket_permission_target("/run/playit/playitd.sock", 1234).unwrap();

        assert_eq!(target.path, "/run/playit/playitd.sock");
        assert_eq!(target.group_name, PLAYIT_SOCKET_GROUP_NAME);
        assert_eq!(target.mode, PLAYIT_SOCKET_MODE);
        assert!(!target.chown_group);
    }

    #[test]
    fn abstract_socket_target_is_ignored() {
        assert_eq!(socket_permission_target("@playitd", 0), None);
    }

    #[test]
    fn windows_pipe_target_is_ignored() {
        assert_eq!(
            socket_permission_target(r"\\.\pipe\playitd-system", 0),
            None
        );
    }
}
