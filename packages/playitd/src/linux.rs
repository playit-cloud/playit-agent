use std::{
    ffi::CString,
    io,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

use playit_ipc::ipc::IpcError;

use crate::manager::ServiceManagerError;

const SYSTEMD_SERVICE_NAME: &str = "playit";
const PLAYIT_SOCKET_GROUP_NAME: &str = "playit";
const PLAYIT_SOCKET_MODE: u32 = 0o660;

pub(crate) fn default_secret_path() -> Option<PathBuf> {
    let path = PathBuf::from("/etc/playit/playit.toml");
    path.exists().then_some(path)
}

pub(crate) fn start_systemd_service() -> Result<(), ServiceManagerError> {
    run_systemctl(&systemd_start_args(), ServiceManagerError::StartFailed)
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

pub(crate) fn stop_systemd_service() -> Result<(), ServiceManagerError> {
    run_systemctl(&systemd_stop_args(), ServiceManagerError::StopFailed)
}

pub(crate) fn configure_socket_permissions(socket_path: &str) -> Result<(), IpcError> {
    let Some(target) = socket_permission_target(socket_path, unsafe { libc::geteuid() as u32 })
    else {
        return Ok(());
    };

    if !Path::new(target.path).exists() {
        return Err(IpcError::BindFailed(io::Error::new(
            io::ErrorKind::NotFound,
            format!("IPC socket {} was not created", target.path),
        )));
    }

    let Some(group_gid) = lookup_group_gid(target.group_name)? else {
        tracing::warn!(
            group = target.group_name,
            socket_path = %target.path,
            "IPC socket group is missing, leaving default socket permissions in place"
        );
        return Ok(());
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinuxSocketPermissionTarget<'a> {
    path: &'a str,
    group_name: &'static str,
    mode: u32,
}

fn socket_permission_target(
    socket_path: &str,
    effective_uid: u32,
) -> Option<LinuxSocketPermissionTarget<'_>> {
    if effective_uid != 0 || socket_path.starts_with('@') || socket_path.starts_with(r"\\.\pipe\") {
        return None;
    }

    Some(LinuxSocketPermissionTarget {
        path: socket_path,
        group_name: PLAYIT_SOCKET_GROUP_NAME,
        mode: PLAYIT_SOCKET_MODE,
    })
}

fn lookup_group_gid(group_name: &str) -> Result<Option<u32>, IpcError> {
    let group_name = CString::new(group_name).map_err(|e| {
        IpcError::BindFailed(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid group name {group_name:?}: {e}"),
        ))
    })?;

    let mut group = std::mem::MaybeUninit::<libc::group>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buf_len = 1024usize;

    loop {
        let mut buf = vec![0u8; buf_len];
        let status = unsafe {
            libc::getgrnam_r(
                group_name.as_ptr(),
                group.as_mut_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut result,
            )
        };

        if status == 0 {
            if result.is_null() {
                return Ok(None);
            }

            let group = unsafe { group.assume_init() };
            return Ok(Some(group.gr_gid));
        }

        if status == libc::ERANGE {
            buf_len *= 2;
            continue;
        }

        return Err(IpcError::BindFailed(io::Error::from_raw_os_error(status)));
    }
}

fn apply_socket_permissions(socket_path: &str, group_gid: u32, mode: u32) -> Result<(), IpcError> {
    let path = Path::new(socket_path);
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|e| {
        IpcError::BindFailed(io::Error::new(
            e.kind(),
            format!("failed to chmod IPC socket {socket_path} to {mode:o}: {e}"),
        ))
    })?;

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
        PLAYIT_SOCKET_GROUP_NAME, PLAYIT_SOCKET_MODE, socket_permission_target,
        systemd_is_active_args, systemd_start_args, systemd_stop_args,
    };

    #[test]
    fn linux_start_targets_playit_unit() {
        assert_eq!(systemd_start_args(), ["start", "playit"]);
    }

    #[test]
    fn linux_is_active_targets_playit_unit() {
        assert_eq!(systemd_is_active_args(), ["is-active", "--quiet", "playit"]);
    }

    #[test]
    fn linux_stop_targets_playit_unit() {
        assert_eq!(systemd_stop_args(), ["stop", "playit"]);
    }

    #[test]
    fn linux_socket_permissions_target_root_filesystem_socket() {
        let target = socket_permission_target("/var/run/playitd.sock", 0)
            .expect("root filesystem socket should be configured");

        assert_eq!(target.group_name, PLAYIT_SOCKET_GROUP_NAME);
        assert_eq!(target.mode, PLAYIT_SOCKET_MODE);
    }

    #[test]
    fn linux_socket_permissions_skip_non_root() {
        assert!(socket_permission_target("/var/run/playitd.sock", 1000).is_none());
    }

    #[test]
    fn linux_socket_permissions_skip_abstract_namespace() {
        assert!(socket_permission_target("@playitd", 0).is_none());
    }
}
