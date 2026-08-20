use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;

use crate::unix::{
    current_process_has_group, current_user_account, effective_gid, effective_uid,
    group_gid_by_name, group_info_by_gid,
};

pub const SOCKET_GROUP_NAME: &str = "playit";
pub const SOCKET_MODE: u32 = 0o660;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketAccessIssue {
    MissingSocket,
    InspectFailed(String),
    NotASocket,
    GroupJoinRequired,
    GroupRefreshRequired,
    PermissionDenied {
        current_uid: u32,
        current_gid: u32,
        socket_uid: u32,
        socket_gid: u32,
        socket_mode: u32,
    },
}

pub fn configure_socket_permissions(socket_path: &str) -> io::Result<()> {
    let Some(target) = socket_permission_target(socket_path, effective_uid()) else {
        return Ok(());
    };
    if !Path::new(target.path).exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("IPC socket {} was not created", target.path),
        ));
    }

    let group_gid = if target.chown_group {
        group_gid_by_name(target.group_name)
    } else {
        None
    };
    apply_socket_permissions(target.path, group_gid, target.mode)
}

pub fn inspect_socket_access(socket_path: &str) -> Option<SocketAccessIssue> {
    let path = Path::new(socket_path);
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Some(SocketAccessIssue::MissingSocket);
        }
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return inspect_parent_access(path)
                .or_else(|| Some(SocketAccessIssue::InspectFailed(error.to_string())));
        }
        Err(error) => return Some(SocketAccessIssue::InspectFailed(error.to_string())),
    };

    if !metadata.file_type().is_socket() {
        return Some(SocketAccessIssue::NotASocket);
    }
    if current_user_can_write(&metadata) {
        return None;
    }

    let socket_gid = metadata.gid();
    if group_info_by_gid(socket_gid)
        .as_ref()
        .map(|group| group.name.as_str())
        == Some(SOCKET_GROUP_NAME)
        && let Some(issue) = group_access_issue(socket_gid)
    {
        return Some(issue);
    }

    Some(SocketAccessIssue::PermissionDenied {
        current_uid: effective_uid(),
        current_gid: effective_gid(),
        socket_uid: metadata.uid(),
        socket_gid,
        socket_mode: metadata.mode() & 0o777,
    })
}

fn inspect_parent_access(path: &Path) -> Option<SocketAccessIssue> {
    let metadata = fs::metadata(path.parent()?).ok()?;
    let gid = metadata.gid();
    let group = group_info_by_gid(gid)?;
    (group.name == SOCKET_GROUP_NAME)
        .then(|| group_access_issue(gid))
        .flatten()
}

fn group_access_issue(target_gid: u32) -> Option<SocketAccessIssue> {
    let account = current_user_account()?;
    let configured = if account.primary_gid == target_gid {
        true
    } else {
        group_info_by_gid(target_gid)?
            .members
            .iter()
            .any(|member| member == &account.username)
    };
    Some(if configured {
        SocketAccessIssue::GroupRefreshRequired
    } else {
        SocketAccessIssue::GroupJoinRequired
    })
}

fn current_user_can_write(metadata: &fs::Metadata) -> bool {
    let uid = effective_uid();
    if uid == 0 {
        return true;
    }
    let mode = metadata.mode();
    if uid == metadata.uid() {
        mode & 0o200 != 0
    } else if current_process_has_group(metadata.gid()) {
        mode & 0o020 != 0
    } else {
        mode & 0o002 != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SocketPermissionTarget<'a> {
    path: &'a str,
    group_name: &'static str,
    mode: u32,
    chown_group: bool,
}

fn socket_permission_target(
    socket_path: &str,
    effective_uid: u32,
) -> Option<SocketPermissionTarget<'_>> {
    if socket_path.starts_with('@') || socket_path.starts_with(r"\\.\pipe\") {
        return None;
    }
    Some(SocketPermissionTarget {
        path: socket_path,
        group_name: SOCKET_GROUP_NAME,
        mode: SOCKET_MODE,
        chown_group: effective_uid == 0,
    })
}

fn apply_socket_permissions(path: &str, group_gid: Option<u32>, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to chmod IPC socket {path} to {mode:o}: {error}"),
        )
    })?;
    let Some(group_gid) = group_gid else {
        return Ok(());
    };
    let path_cstr = CString::new(Path::new(path).as_os_str().as_bytes()).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid IPC socket path {path:?}: {error}"),
        )
    })?;
    if unsafe { libc::chown(path_cstr.as_ptr(), u32::MAX, group_gid) } != 0 {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "failed to chown IPC socket {path} to group gid {group_gid}: {}",
                io::Error::last_os_error()
            ),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_socket_permissions_are_stable() {
        let target = socket_permission_target("/run/playit/playitd.sock", 0).unwrap();
        assert_eq!(target.group_name, "playit");
        assert_eq!(target.mode, 0o660);
        assert!(target.chown_group);
    }

    #[test]
    fn non_filesystem_endpoints_are_ignored() {
        assert_eq!(socket_permission_target("@playitd", 0), None);
        assert_eq!(
            socket_permission_target(r"\\.\pipe\playitd-system", 0),
            None
        );
    }
}
