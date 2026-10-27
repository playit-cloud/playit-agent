//! Windows integration: the service host, service self-configuration, and small shell
//! helpers shared by the CLI and tray.

mod legacy_secret;
mod service_config;
pub mod service_host;

use std::io::{self, Write};
use std::path::Path;

use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use playit_ipc::ipc::IpcError;
use widestring::U16CString;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::Console::GetConsoleProcessList;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// SYSTEM, Administrators and every signed-in user may talk to the service without
/// elevation. Users get read/write (0x12019b) but not FILE_CREATE_PIPE_INSTANCE, so they
/// cannot add server instances to the pipe and intercept other users' clients.
const SERVICE_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12019b;;;AU)";

/// Pipes other than the service's (portable mode, manual `--socket-path`) belong to the
/// user who started the daemon.
const PRIVATE_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;OW)";

pub const DASHBOARD_URL: &str = "https://playit.gg/account/tunnels";

pub fn pipe_security_descriptor(socket_path: &str) -> Result<SecurityDescriptor, IpcError> {
    let sddl = if socket_path.eq_ignore_ascii_case(playit_ipc::ipc::get_default_socket_path()) {
        SERVICE_PIPE_SDDL
    } else {
        PRIVATE_PIPE_SDDL
    };
    let sddl = U16CString::from_str(sddl).map_err(|error| {
        IpcError::BindFailed(io::Error::new(io::ErrorKind::InvalidInput, error))
    })?;
    SecurityDescriptor::deserialize(&sddl).map_err(IpcError::BindFailed)
}

/// Opens a URL in the default browser.
pub fn open_url(url: &str) -> io::Result<()> {
    shell_open(url)
}

/// Opens a folder in Explorer. The folder is not created: ProgramData folders must be
/// created by the service so users cannot own them.
pub fn open_folder(path: &Path) -> io::Result<()> {
    if !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist yet", path.display()),
        ));
    }
    shell_open(&path.display().to_string())
}

fn shell_open(target: &str) -> io::Result<()> {
    let operation = U16CString::from_str("open").expect("static string has no nul");
    let target = U16CString::from_str(target)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

    // Some URL handlers are COM based and need an initialized apartment on this thread.
    let com_initialized = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    } >= 0;

    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };

    if com_initialized {
        unsafe { CoUninitialize() };
    }

    // ShellExecuteW reports success with a value greater than 32.
    if result as usize > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "ShellExecute failed with code {}",
            result as usize
        )))
    }
}

/// Holds a named mutex for as long as it lives; used to allow one instance per session.
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    /// Returns `None` when another process already holds `name`.
    pub fn acquire(name: &str) -> io::Result<Option<Self>> {
        let name = U16CString::from_str(name)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(Self(handle)))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

unsafe impl Send for SingleInstance {}
unsafe impl Sync for SingleInstance {}

/// True when this process is the only one attached to its console, meaning Windows
/// created the console for us (double-click, Start menu, tray) and will close it as
/// soon as we exit.
pub fn owns_console_window() -> bool {
    let mut process_ids = [0u32; 4];
    let count =
        unsafe { GetConsoleProcessList(process_ids.as_mut_ptr(), process_ids.len() as u32) };
    count == 1
}

/// Last-resort log for failures that happen before the daemon's tracing is set up.
pub(crate) fn write_service_log(message: &str) {
    let path = crate::windows_service_log_dir().join("playitd-service.log");
    let _ = std::fs::create_dir_all(crate::windows_service_log_dir());
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let _ = writeln!(file, "[{timestamp}] {message}");
    }
}
