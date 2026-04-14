use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
    SetConsoleOutputCP, WriteConsoleW,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

static DEBUG_CONSOLE: AtomicBool = AtomicBool::new(false);

pub(super) struct SingleInstanceGuard {
    handle: HANDLE,
}

impl SingleInstanceGuard {
    pub(super) fn new(name: &str) -> Result<Option<Self>, String> {
        let name = wide(name);
        let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(last_error("failed to create tray mutex"));
        }

        let last_error = unsafe { GetLastError() };
        if last_error != 0 {
            const ERROR_ALREADY_EXISTS: u32 = 183;
            if last_error == ERROR_ALREADY_EXISTS {
                unsafe {
                    CloseHandle(handle);
                }
                return Ok(None);
            }
        }

        Ok(Some(Self { handle }))
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

pub(super) fn init_debug_console_from_args() {
    let enabled = std::env::args_os().any(|arg| arg == "--debug-console");
    if !enabled {
        return;
    }

    DEBUG_CONSOLE.store(true, Ordering::Relaxed);

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            let _ = AllocConsole();
        }
        let _ = SetConsoleOutputCP(65001);
    }

    std::panic::set_hook(Box::new(|panic_info| {
        debug_log(&format!("panic: {panic_info}"));
    }));

    debug_log("debug console enabled");
}

pub(super) fn show_error(title: &str, message: &str) {
    debug_log(&format!("{title}: {message}"));
    let title = wide(title);
    let message = wide(message);
    unsafe {
        let _ = MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub(super) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(super) fn last_error(context: &str) -> String {
    format!("{context} (Win32 error {})", unsafe { GetLastError() })
}

pub(super) fn debug_log(message: &str) {
    if !DEBUG_CONSOLE.load(Ordering::Relaxed) {
        return;
    }

    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return;
    }

    let line = wide(&format!("{message}\r\n"));
    let mut written = 0u32;
    unsafe {
        let _ = WriteConsoleW(
            handle,
            line.as_ptr(),
            line.len().saturating_sub(1) as u32,
            &mut written,
            null(),
        );
    }
}
