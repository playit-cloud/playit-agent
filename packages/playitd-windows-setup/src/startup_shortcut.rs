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
use windows::core::{GUID, Interface, PCWSTR};

const TRAY_SHORTCUT_NAME: &str = "Playit Tray.lnk";
const TRAY_SHORTCUT_DESCRIPTION: &str =
    "Shows the Playit tray icon when the background service is running.";

struct ComInitialization {
    should_uninitialize: bool,
}

impl Drop for ComInitialization {
    fn drop(&mut self) {
        if self.should_uninitialize {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

pub(crate) fn remove_startup_shortcut() -> Result<(), String> {
    let shortcut_path = startup_shortcut_path()?;

    if !shortcut_path.exists() {
        return Ok(());
    }

    fs::remove_file(&shortcut_path).map_err(|error| {
        format!(
            "Failed to delete startup shortcut at {}: {error}",
            shortcut_path.display()
        )
    })
}

pub(crate) fn ensure_startup_shortcut() -> Result<(), String> {
    let shortcut_path = startup_shortcut_path()?;
    let setup_path = std::env::current_exe()
        .map_err(|error| format!("Failed to resolve playitd-windows-setup.exe path: {error}"))?;
    let working_directory = setup_path.parent().ok_or_else(|| {
        format!(
            "Failed to resolve the working directory for {}",
            setup_path.display()
        )
    })?;
    let tray_path = working_directory.join("playitd-tray.exe");

    if let Some(parent) = shortcut_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create the current user's Startup folder at {}: {error}",
                parent.display()
            )
        })?;
    }

    let _com = initialize_com()?;
    create_startup_shortcut(&tray_path, working_directory, &shortcut_path)
}

fn startup_shortcut_path() -> Result<PathBuf, String> {
    known_folder_shortcut_path(&FOLDERID_Startup, "the current user's Startup folder")
}

fn known_folder_shortcut_path(folder_id: &GUID, folder_name: &str) -> Result<PathBuf, String> {
    unsafe {
        let wide_path = SHGetKnownFolderPath(folder_id, KF_FLAG_DEFAULT, None)
            .map_err(|error| format!("Failed to resolve {folder_name}: {error}"))?;

        if wide_path.is_null() {
            return Err(format!("{folder_name} path was empty"));
        }

        let path = wide_path
            .to_string()
            .map_err(|error| format!("Failed to read {folder_name} path: {error}"))?;
        CoTaskMemFree(Some(wide_path.0.cast()));

        Ok(PathBuf::from(path).join(TRAY_SHORTCUT_NAME))
    }
}

fn create_startup_shortcut(
    tray_path: &Path,
    working_directory: &Path,
    shortcut_path: &Path,
) -> Result<(), String> {
    unsafe {
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|error| format!("Failed to create the ShellLink COM object: {error}"))?;

        shell_link
            .SetPath(path_pcwstr(tray_path)?.as_pcwstr())
            .map_err(|error| {
                format!(
                    "Failed to set the tray shortcut target to {}: {error}",
                    tray_path.display()
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
            .Save(path_pcwstr(shortcut_path)?.as_pcwstr(), true)
            .map_err(|error| {
                format!(
                    "Failed to save the startup shortcut at {}: {error}",
                    shortcut_path.display()
                )
            })
    }
}

fn initialize_com() -> Result<ComInitialization, String> {
    unsafe {
        let result = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if result == S_OK || result == S_FALSE {
            return Ok(ComInitialization {
                should_uninitialize: true,
            });
        }
        if result == RPC_E_CHANGED_MODE {
            return Ok(ComInitialization {
                should_uninitialize: false,
            });
        }

        Err(format!(
            "Failed to initialize COM for the tray shortcut helper (HRESULT {:#x})",
            result.0
        ))
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
