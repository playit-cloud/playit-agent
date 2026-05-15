use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr::{NonNull, null_mut};

use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use playit_ipc::ipc::IpcError;
use widestring::U16CString;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::windows_service_data_dir;

pub fn installed_user_sid_path() -> PathBuf {
    windows_service_data_dir().join("installed_user.sid")
}

pub fn read_installed_user_sid() -> Option<String> {
    let content = match std::fs::read_to_string(installed_user_sid_path()) {
        Ok(content) => content,
        Err(error) => {
            tracing::debug!("failed to read installed user SID: {error}");
            return None;
        }
    };

    match normalize_sid(content.trim()) {
        Some(sid) => Some(sid.to_string()),
        None => {
            tracing::warn!("installed user SID file is invalid, ignoring it");
            None
        }
    }
}

pub fn write_current_user_sid() -> io::Result<PathBuf> {
    let sid = current_process_user_sid()?;
    let path = installed_user_sid_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format!("{sid}\n"))?;
    Ok(path)
}

pub fn current_process_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let token = Handle::new(token).ok_or_else(io::Error::last_os_error)?;
    token_user_sid_string(token.raw())
}

pub fn restricted_pipe_security_descriptor() -> Result<SecurityDescriptor, IpcError> {
    let mut user_sid = read_installed_user_sid();
    if user_sid.is_none() {
        match current_process_user_sid() {
            Ok(sid) => user_sid = Some(sid),
            Err(error) => {
                tracing::warn!("failed to read current process SID for IPC ACL fallback: {error}");
            }
        }
    }

    let sddl = pipe_security_sddl(user_sid.as_deref());
    let sddl = U16CString::from_str(&sddl).map_err(|error| {
        IpcError::BindFailed(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid pipe security descriptor string: {error}"),
        ))
    })?;

    SecurityDescriptor::deserialize(&sddl).map_err(IpcError::BindFailed)
}

fn token_user_sid_string(token: HANDLE) -> io::Result<String> {
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
    }

    if needed == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut buffer = vec![0u8; needed as usize];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let token_user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    sid_to_string(token_user.User.Sid)
}

fn sid_to_string(sid: *mut std::ffi::c_void) -> io::Result<String> {
    let mut string_sid = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut string_sid) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let string_sid = LocalString::new(string_sid).ok_or_else(io::Error::last_os_error)?;
    Ok(string_sid.to_string())
}

fn normalize_sid(sid: &str) -> Option<&str> {
    if !sid.starts_with("S-1-") {
        return None;
    }

    if sid
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | ';'))
    {
        return None;
    }

    if !sid
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, 'S' | '-'))
    {
        return None;
    }

    let mut parts = sid.split('-');
    if parts.next() != Some("S") || parts.next() != Some("1") {
        return None;
    }
    if !parts.all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }

    Some(sid)
}

fn pipe_security_sddl(user_sid: Option<&str>) -> String {
    let mut sddl = String::from("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)");
    if let Some(user_sid) = user_sid.and_then(normalize_sid) {
        sddl.push_str("(A;;GA;;;");
        sddl.push_str(user_sid);
        sddl.push(')');
    }
    sddl
}

struct Handle(NonNull<std::ffi::c_void>);

impl Handle {
    fn new(handle: HANDLE) -> Option<Self> {
        NonNull::new(handle).map(Self)
    }

    fn raw(&self) -> HANDLE {
        self.0.as_ptr()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.raw());
        }
    }
}

struct LocalString(NonNull<u16>);

impl LocalString {
    fn new(ptr: *mut u16) -> Option<Self> {
        NonNull::new(ptr).map(Self)
    }

    fn to_string(&self) -> String {
        let mut len = 0;
        unsafe {
            while *self.0.as_ptr().add(len) != 0 {
                len += 1;
            }
            OsString::from_wide(std::slice::from_raw_parts(self.0.as_ptr(), len))
                .to_string_lossy()
                .into_owned()
        }
    }
}

impl Drop for LocalString {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(self.0.as_ptr().cast());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_sid, pipe_security_sddl};

    #[test]
    fn pipe_sddl_allows_service_admins_authenticated_users_and_installed_user() {
        let sddl = pipe_security_sddl(Some("S-1-5-21-1-2-3-1001"));

        assert!(sddl.contains("(A;;GA;;;SY)"));
        assert!(sddl.contains("(A;;GA;;;BA)"));
        assert!(sddl.contains("(A;;GA;;;AU)"));
        assert!(sddl.contains("(A;;GA;;;S-1-5-21-1-2-3-1001)"));
        assert!(!sddl.contains("WD"));
        assert!(!sddl.contains("BU"));
    }

    #[test]
    fn sid_validation_rejects_sddl_breakout_characters() {
        assert_eq!(
            normalize_sid("S-1-5-21-1-2-3-1001"),
            Some("S-1-5-21-1-2-3-1001")
        );
        assert_eq!(normalize_sid(""), None);
        assert_eq!(normalize_sid("S-1-"), None);
        assert_eq!(normalize_sid(" S-1-5-21-1-2-3-1001"), None);
        assert_eq!(normalize_sid("S-1-5-21-1-2-3-1001 "), None);
        assert_eq!(normalize_sid("S-1-5-21-1-2-3-1001)"), None);
        assert_eq!(normalize_sid("S-1-5-21-1-2-3-1001;"), None);
        assert_eq!(normalize_sid("S-1-5-21-1-2-3-1001("), None);
    }
}
