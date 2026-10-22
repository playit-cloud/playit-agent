//! The installer registers the tray in HKLM\...\Run for every user. Each user can opt out
//! with "Launch at Login", which is stored per user and checked on `--autostart`.

use std::path::PathBuf;
use std::ptr::null_mut;

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Startup, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW,
};

use super::util::{debug_log, wide};

const SETTINGS_KEY: &str = r"Software\DevelopedMethodsLLC\playit_gg\Tray";
const LAUNCH_AT_LOGIN_VALUE: &str = "LaunchAtLogin";
/// Created in the Startup folder by 1.0.x installers; replaced by the Run key.
const LEGACY_STARTUP_SHORTCUT: &str = "Playit Tray.lnk";

pub(super) fn launch_at_login_enabled() -> bool {
    let key = wide(SETTINGS_KEY);
    let value = wide(LAUNCH_AT_LOGIN_VALUE);
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };

    // Missing means the user never opted out.
    status != ERROR_SUCCESS || data != 0
}

pub(super) fn set_launch_at_login(enabled: bool) -> Result<(), String> {
    let key = wide(SETTINGS_KEY);
    let value = wide(LAUNCH_AT_LOGIN_VALUE);
    let data = u32::from(enabled);
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            REG_DWORD,
            (&data as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };

    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!(
            "Could not save the Launch at Login setting: {}",
            std::io::Error::from_raw_os_error(status as i32)
        ))
    }
}

pub(super) fn remove_legacy_startup_shortcut() {
    let Some(path) = startup_folder().map(|folder| folder.join(LEGACY_STARTUP_SHORTCUT)) else {
        return;
    };
    if path.exists() {
        match std::fs::remove_file(&path) {
            Ok(()) => debug_log(&format!(
                "removed legacy startup shortcut {}",
                path.display()
            )),
            Err(error) => debug_log(&format!(
                "failed to remove legacy startup shortcut {}: {error}",
                path.display()
            )),
        }
    }
}

fn startup_folder() -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Startup, KF_FLAG_DEFAULT, None).ok()?;
        let folder = path.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(path.0.cast()));
        folder
    }
}
