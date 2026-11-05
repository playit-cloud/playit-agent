//! Fits the window into the installed desktop app: one playit window per user, with the tray
//! (Windows) or menu bar (macOS) icon running beside it.

/// The window title, which is also how a second launch finds the first window on Windows.
pub const WINDOW_TITLE: &str = "playit";

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
