use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::ptr::{NonNull, null_mut};

use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use widestring::U16CString;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::paths::windows_installed_user_sid_path;

pub fn read_installed_user_sid() -> Option<String> {
    let content = std::fs::read_to_string(windows_installed_user_sid_path()).ok()?;
    normalize_sid(content.trim()).map(str::to_owned)
}

pub fn write_current_user_sid() -> io::Result<std::path::PathBuf> {
    let sid = current_process_user_sid()?;
    let path = windows_installed_user_sid_path();
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

pub fn restricted_pipe_security_descriptor() -> io::Result<SecurityDescriptor> {
    let user_sid = read_installed_user_sid().or_else(|| current_process_user_sid().ok());
    let sddl = U16CString::from_str(pipe_security_sddl(user_sid.as_deref()))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    SecurityDescriptor::deserialize(&sddl)
}

pub fn normalize_sid(sid: &str) -> Option<&str> {
    if !sid.starts_with("S-1-")
        || sid
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '(' | ')' | ';'))
        || !sid
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, 'S' | '-'))
    {
        return None;
    }
    let mut parts = sid.split('-');
    if parts.next() != Some("S") || parts.next() != Some("1") {
        return None;
    }
    parts
        .all(|part| !part.is_empty() && part.chars().all(|character| character.is_ascii_digit()))
        .then_some(sid)
}

pub fn pipe_security_sddl(user_sid: Option<&str>) -> String {
    let mut sddl = String::from("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)");
    if let Some(user_sid) = user_sid.and_then(normalize_sid) {
        sddl.push_str("(A;;GA;;;");
        sddl.push_str(user_sid);
        sddl.push(')');
    }
    sddl
}

fn token_user_sid_string(token: HANDLE) -> io::Result<String> {
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let word_size = std::mem::size_of::<usize>();
    let word_count = (needed as usize).div_ceil(word_size);
    let mut buffer = vec![0usize; word_count];
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
        unsafe { CloseHandle(self.raw()) };
    }
}

struct LocalString(NonNull<u16>);

impl LocalString {
    fn new(pointer: *mut u16) -> Option<Self> {
        NonNull::new(pointer).map(Self)
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
    use super::*;

    #[test]
    fn sid_validation_and_pipe_acl_are_stable() {
        let sid = "S-1-5-21-1-2-3-1001";
        assert_eq!(normalize_sid(sid), Some(sid));
        assert_eq!(normalize_sid("S-1-5-21;)"), None);
        let sddl = pipe_security_sddl(Some(sid));
        assert!(sddl.contains("(A;;GA;;;SY)"));
        assert!(sddl.contains("(A;;GA;;;BA)"));
        assert!(sddl.contains("(A;;GA;;;AU)"));
        assert!(sddl.contains(sid));
        assert!(!sddl.contains("WD"));
    }
}
