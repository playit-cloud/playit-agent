//! Fits the window into the installed desktop app: one playit window per user, with the tray
//! (Windows) or menu bar (macOS) icon running beside it, and a launcher entry on Linux.

/// The window title, which is also how a second launch finds the first window on Windows.
pub const WINDOW_TITLE: &str = "playit";

/// The Wayland app ID and X11 window class. Linux desktops match it to the launcher entry of
/// the same name to show the window's icon.
pub const APP_ID: &str = "gg.playit.gui";

/// Holds the claim on being the only playit window. Released when dropped.
pub struct SingleInstance {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

/// Returns `None` when another playit window is already open, after bringing it to the front.
/// macOS needs no lock: opening playit.app again activates the running copy.
pub fn claim_single_instance() -> Option<SingleInstance> {
    #[cfg(windows)]
    {
        windows::claim_single_instance()
    }
    #[cfg(not(windows))]
    {
        Some(SingleInstance {})
    }
}

/// Starts the tray or menu bar icon if playit was installed with one and it isn't running.
/// It keeps a single instance itself, so a second copy exits right away.
pub fn start_tray() {
    #[cfg(any(windows, target_os = "macos"))]
    {
        let Some(tray) = tray_executable() else {
            return;
        };
        let mut command = std::process::Command::new(&tray);
        // On macOS the menu bar app opens this window unless told it was started for it.
        #[cfg(target_os = "macos")]
        command.arg("--background");
        if let Err(error) = command.spawn() {
            tracing::warn!("failed to start {}: {error}", tray.display());
        }
    }
}

/// Adds playit to the Linux app launcher for this user, pointing at the running executable,
/// so the window gets the playit icon. Skipped when a system-wide entry already exists.
#[cfg(target_os = "linux")]
pub fn register_launcher_entry() {
    if let Err(error) = linux::register_launcher_entry() {
        tracing::warn!("failed to add playit to the app launcher: {error}");
    }
}

#[cfg(windows)]
fn tray_executable() -> Option<std::path::PathBuf> {
    let tray = std::env::current_exe()
        .ok()?
        .with_file_name("playitd-tray.exe");
    tray.is_file().then_some(tray)
}

#[cfg(target_os = "macos")]
fn tray_executable() -> Option<std::path::PathBuf> {
    let tray = playitd::macos::current_app_bundle()?.join(playitd::macos::TRAY_HELPER_EXECUTABLE);
    tray.is_file().then_some(tray)
}

#[cfg(windows)]
mod windows {
    use std::ptr::null;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    use super::{SingleInstance, WINDOW_TITLE};

    /// GPUI registers every window under this class.
    const GPUI_WINDOW_CLASS: &str = "Zed::Window";

    pub(super) fn claim_single_instance() -> Option<SingleInstance> {
        let name = wide("Local\\playit-gui");
        let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if handle.is_null() {
            // Without the mutex the worst case is a second window, so carry on.
            return Some(SingleInstance { handle });
        }
        if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            return Some(SingleInstance { handle });
        }
        unsafe { CloseHandle(handle) };
        focus_existing_window();
        None
    }

    fn focus_existing_window() {
        let class = wide(GPUI_WINDOW_CLASS);
        let title = wide(WINDOW_TITLE);
        unsafe {
            let window = FindWindowW(class.as_ptr(), title.as_ptr());
            if window.is_null() {
                return;
            }
            if IsIconic(window) != 0 {
                ShowWindow(window, SW_RESTORE);
            }
            SetForegroundWindow(window);
        }
    }

    impl Drop for SingleInstance {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                unsafe { CloseHandle(self.handle) };
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::path::{Path, PathBuf};

    use super::APP_ID;

    /// The same logo as the Windows and macOS app icons.
    const ICON: &[u8] = include_bytes!("../assets/images/app-icon-256.png");

    pub(super) fn register_launcher_entry() -> io::Result<()> {
        let entry_name = format!("{APP_ID}.desktop");
        if system_data_dirs()
            .iter()
            .any(|dir| dir.join("applications").join(&entry_name).is_file())
        {
            return Ok(());
        }
        let Some(data_dir) = dirs::data_dir() else {
            return Ok(());
        };

        let exe = std::env::current_exe()?;
        write_if_changed(
            &data_dir
                .join("icons/hicolor/256x256/apps")
                .join(format!("{APP_ID}.png")),
            ICON,
        )?;
        write_if_changed(
            &data_dir.join("applications").join(entry_name),
            desktop_entry(&exe).as_bytes(),
        )
    }

    fn system_data_dirs() -> Vec<PathBuf> {
        let dirs = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
        dirs.split(':')
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .collect()
    }

    fn desktop_entry(exe: &Path) -> String {
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=playit\n\
             GenericName=Tunnel manager\n\
             Comment=Makes local ports public. Put your game servers online.\n\
             Exec={exec}\n\
             Icon={APP_ID}\n\
             StartupWMClass={APP_ID}\n\
             Terminal=false\n\
             Categories=Network;\n",
            exec = exec_argument(&exe.to_string_lossy()),
        )
    }

    /// Quotes a path for a desktop entry's `Exec` key, following the Desktop Entry spec.
    fn exec_argument(path: &str) -> String {
        let mut quoted = String::from("\"");
        for ch in path.chars() {
            match ch {
                '"' | '`' | '$' | '\\' => {
                    quoted.push('\\');
                    quoted.push(ch);
                }
                '%' => quoted.push_str("%%"),
                _ => quoted.push(ch),
            }
        }
        quoted.push('"');
        quoted
    }

    fn write_if_changed(path: &Path, contents: &[u8]) -> io::Result<()> {
        if std::fs::read(path).is_ok_and(|current| current == contents) {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn exec_paths_are_quoted_and_escaped() {
            assert_eq!(
                exec_argument("/opt/playit/playit-gui"),
                "\"/opt/playit/playit-gui\""
            );
            assert_eq!(
                exec_argument("/home/a b/$x/100%/\"q\""),
                "\"/home/a b/\\$x/100%%/\\\"q\\\"\""
            );
        }

        #[test]
        fn entry_matches_the_window_app_id() {
            let entry = desktop_entry(Path::new("/opt/playit/playit-gui"));
            assert!(entry.contains("Icon=gg.playit.gui\n"));
            assert!(entry.contains("StartupWMClass=gg.playit.gui\n"));
            assert!(entry.starts_with("[Desktop Entry]\nType=Application\n"));
        }
    }
}
