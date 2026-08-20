use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IPersistFile,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Startup, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink,
};
use windows::core::{Interface, PCWSTR};

use crate::paths::WINDOWS_TRAY_SHORTCUT_NAME;

const TRAY_SHORTCUT_DESCRIPTION: &str =
    "Shows the Playit tray icon when the background service is running.";

pub fn ensure_tray_startup_shortcut(tray_path: &Path) -> Result<PathBuf, String> {
    let shortcut_path = tray_startup_shortcut_path()?;
    let working_directory = tray_path.parent().ok_or_else(|| {
        format!(
            "Failed to resolve the working directory for {}",
            tray_path.display()
        )
    })?;
    if let Some(parent) = shortcut_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create the current user's Startup folder at {}: {error}",
                parent.display()
            )
        })?;
    }

    let _com = initialize_com()?;
    create_shortcut(tray_path, working_directory, &shortcut_path)?;
    Ok(shortcut_path)
}

pub fn remove_tray_startup_shortcut() -> Result<(), String> {
    let path = tray_startup_shortcut_path()?;
    if path.exists() {
        fs::remove_file(&path).map_err(|error| {
            format!(
                "Failed to delete startup shortcut at {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

pub fn tray_startup_shortcut_exists() -> Result<bool, String> {
    Ok(tray_startup_shortcut_path()?.exists())
}

pub fn tray_startup_shortcut_path() -> Result<PathBuf, String> {
    unsafe {
        let wide_path =
            SHGetKnownFolderPath(&FOLDERID_Startup, KF_FLAG_DEFAULT, None).map_err(|error| {
                format!("Failed to resolve the current user's Startup folder: {error}")
            })?;
        if wide_path.is_null() {
            return Err("the current user's Startup folder path was empty".to_owned());
        }
        let path = wide_path.to_string().map_err(|error| {
            format!("Failed to read the current user's Startup folder: {error}")
        })?;
        CoTaskMemFree(Some(wide_path.0.cast()));
        Ok(PathBuf::from(path).join(WINDOWS_TRAY_SHORTCUT_NAME))
    }
}

fn create_shortcut(target: &Path, working_directory: &Path, path: &Path) -> Result<(), String> {
    unsafe {
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|error| format!("Failed to create the ShellLink COM object: {error}"))?;
        shell_link
            .SetPath(path_pcwstr(target)?.as_pcwstr())
            .map_err(|error| {
                format!(
                    "Failed to set the tray shortcut target to {}: {error}",
                    target.display()
                )
            })?;
        shell_link
            .SetWorkingDirectory(path_pcwstr(working_directory)?.as_pcwstr())
            .map_err(|error| {
                format!(
                    "Failed to set the tray shortcut working directory to {}: {error}",
                    working_directory.display()
                )
            })?;
        shell_link
            .SetDescription(wide(TRAY_SHORTCUT_DESCRIPTION).as_pcwstr())
            .map_err(|error| format!("Failed to set the tray shortcut description: {error}"))?;
        let persist_file: IPersistFile = shell_link.cast().map_err(|error| {
            format!("Failed to query the tray shortcut persistence interface: {error}")
        })?;
        persist_file
            .Save(path_pcwstr(path)?.as_pcwstr(), true)
            .map_err(|error| {
                format!(
                    "Failed to save the startup shortcut at {}: {error}",
                    path.display()
                )
            })
    }
}

struct ComInitialization(bool);

impl Drop for ComInitialization {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn initialize_com() -> Result<ComInitialization, String> {
    unsafe {
        let result = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if result == S_OK || result == S_FALSE {
            Ok(ComInitialization(true))
        } else if result == RPC_E_CHANGED_MODE {
            Ok(ComInitialization(false))
        } else {
            Err(format!(
                "Failed to initialize COM for the tray shortcut helper (HRESULT {:#x})",
                result.0
            ))
        }
    }
}

struct WideString(Vec<u16>);

impl WideString {
    fn as_pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }
}

fn path_pcwstr(path: &Path) -> Result<WideString, String> {
    Ok(WideString(
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect(),
    ))
}

fn wide(value: &str) -> WideString {
    WideString(value.encode_utf16().chain(std::iter::once(0)).collect())
}
