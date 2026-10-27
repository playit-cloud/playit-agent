//! playit 0.17 stored its secret in each user's %LOCALAPPDATA%. The first time the
//! service starts without a secret, adopt the most recently written valid 0.17 secret on
//! this machine so upgrading users keep their agent. This runs once: after a user resets
//! their agent, an old 0.17 secret must not come back.

use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::time::SystemTime;

use widestring::{U16CStr, U16CString};
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW,
    RegGetValueW, RegOpenKeyExW,
};

use crate::daemon::parse_secret_file;
use crate::{windows_service_data_dir, windows_service_secret_path};

const PROFILE_LIST_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";
const MIGRATION_MARKER: &str = "legacy-secret-migration.done";

/// Returns the legacy secret path that was copied, if any.
pub(super) fn migrate_if_missing() -> Result<Option<PathBuf>, String> {
    let marker = windows_service_data_dir().join(MIGRATION_MARKER);
    if marker.exists() {
        return Ok(None);
    }

    let result = migrate(&windows_service_secret_path());
    if result.is_ok() {
        let _ = std::fs::write(&marker, b"");
    }
    result
}

fn migrate(target: &Path) -> Result<Option<PathBuf>, String> {
    if target.exists() {
        return Ok(None);
    }

    let newest = profile_directories()?
        .into_iter()
        .map(|profile| legacy_secret_path(&profile))
        .filter_map(|path| {
            let content = std::fs::read_to_string(&path).ok()?;
            parse_secret_file(&content).ok()?;
            let modified = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((modified, path, content))
        })
        .max_by_key(|(modified, _, _)| *modified);

    let Some((_, source, content)) = newest else {
        return Ok(None);
    };

    std::fs::write(target, content)
        .map_err(|error| format!("failed to write {}: {error}", target.display()))?;
    Ok(Some(source))
}

fn legacy_secret_path(profile_dir: &Path) -> PathBuf {
    profile_dir
        .join("AppData")
        .join("Local")
        .join("playit_gg")
        .join("playit.toml")
}

/// Profile directories of real user accounts (SIDs under S-1-5-21) from the registry.
fn profile_directories() -> Result<Vec<PathBuf>, String> {
    let key = RegistryKey::open(PROFILE_LIST_KEY)?;
    let mut directories = Vec::new();

    for index in 0.. {
        let mut name = [0u16; 256];
        let mut name_len = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            return Err(format!("failed to enumerate user profiles: error {status}"));
        }

        let sid = String::from_utf16_lossy(&name[..name_len as usize]);
        if !sid.starts_with("S-1-5-21-") {
            continue;
        }
        if let Some(directory) = key.read_string(&sid, "ProfileImagePath") {
            directories.push(PathBuf::from(directory));
        }
    }

    Ok(directories)
}

struct RegistryKey(HKEY);

impl RegistryKey {
    fn open(path: &str) -> Result<Self, String> {
        let path_wide = U16CString::from_str(path).map_err(|error| error.to_string())?;
        let mut key: HKEY = null_mut();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                path_wide.as_ptr(),
                0,
                // Read the native view even from the 32-bit build on 64-bit Windows.
                KEY_READ | KEY_WOW64_64KEY,
                &mut key,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("failed to open HKLM\\{path}: error {status}"));
        }
        Ok(Self(key))
    }

    /// Reads a string value; REG_EXPAND_SZ values come back with variables expanded.
    fn read_string(&self, subkey: &str, value: &str) -> Option<String> {
        let subkey = U16CString::from_str(subkey).ok()?;
        let value = U16CString::from_str(value).ok()?;
        let mut buffer = vec![0u16; 1024];
        let mut byte_len = (buffer.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                self.0,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut byte_len,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let text = U16CStr::from_slice_truncate(&buffer).ok()?;
        Some(text.to_string_lossy())
    }
}

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
