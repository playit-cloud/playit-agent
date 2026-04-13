#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod windows_tray {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::ptr::{null, null_mut};

    use playitd::manager::{INSTALLED_SERVICE_LABEL, stop_installed_service};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT,
        SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
    };
    use windows_sys::Win32::System::Threading::{CREATE_NEW_CONSOLE, CreateMutexW};
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CREATESTRUCTW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW, GetWindowLongPtrW, HICON,
        IDI_APPLICATION, KillTimer, LoadIconW, MB_ICONERROR, MB_OK, MF_STRING, MSG, MessageBoxW,
        PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
        TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP,
        WM_COMMAND, WM_CREATE, WM_DESTROY, WM_LBUTTONUP, WM_NCCREATE, WM_NCDESTROY, WM_NULL,
        WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
    };

    const TRAY_CALLBACK_MESSAGE: u32 = WM_APP + 1;
    const TRAY_ICON_ID: u32 = 1;
    const POLL_TIMER_ID: usize = 1;
    const POLL_INTERVAL_MS: u32 = 2_000;
    const MENU_OPEN_STATUS: usize = 1001;
    const MENU_STOP_SERVICE: usize = 1002;

    pub fn run() -> Result<(), String> {
        let _instance_guard = match SingleInstanceGuard::new("Local\\playitd-tray")? {
            Some(guard) => guard,
            None => return Ok(()),
        };

        let class_name = wide("PlayitTrayWindowClass");
        let window_title = wide("playit tray");

        let hinstance = unsafe { GetModuleHandleW(null()) };
        if hinstance.is_null() {
            return Err(last_error("failed to get module handle"));
        }

        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: hinstance,
            lpszClassName: class_name.as_ptr(),
            ..unsafe { zeroed() }
        };

        if unsafe { RegisterClassW(&wnd_class) } == 0 {
            return Err(last_error("failed to register tray window class"));
        }

        let state = Box::new(AppState {
            tray_visible: false,
            tray_icon: unsafe { LoadIconW(null_mut(), IDI_APPLICATION) },
        });
        let state_ptr = Box::into_raw(state);

        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                window_title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                hinstance,
                state_ptr.cast::<c_void>(),
            )
        };

        if hwnd.is_null() {
            unsafe {
                drop(Box::from_raw(state_ptr));
            }
            return Err(last_error("failed to create tray window"));
        }

        let mut message = unsafe { zeroed::<MSG>() };
        loop {
            let result = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
            if result == -1 {
                return Err(last_error("tray message loop failed"));
            }
            if result == 0 {
                break;
            }

            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        Ok(())
    }

    struct AppState {
        tray_visible: bool,
        tray_icon: HICON,
    }

    struct SingleInstanceGuard {
        handle: HANDLE,
    }

    impl SingleInstanceGuard {
        fn new(name: &str) -> Result<Option<Self>, String> {
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

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_NCCREATE => {
                let create_struct = lparam as *const CREATESTRUCTW;
                if !create_struct.is_null() {
                    let state = (*create_struct).lpCreateParams as *mut AppState;
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
                }
                return 1;
            }
            WM_CREATE => {
                let _ = SetTimer(hwnd, POLL_TIMER_ID, POLL_INTERVAL_MS, None);
                refresh_tray_visibility(hwnd);
                return 0;
            }
            WM_TIMER => {
                if wparam == POLL_TIMER_ID {
                    refresh_tray_visibility(hwnd);
                }
                return 0;
            }
            WM_COMMAND => {
                match menu_id(wparam) {
                    MENU_OPEN_STATUS => {
                        if let Err(error) = launch_status_window() {
                            show_error("Failed to open playit status", &error);
                        }
                    }
                    MENU_STOP_SERVICE => {
                        if let Err(error) = stop_installed_service()
                            .map_err(|error| format!("Failed to stop playitd service: {error}"))
                        {
                            show_error("Failed to stop playit service", &error);
                        }
                    }
                    _ => {}
                }
                return 0;
            }
            TRAY_CALLBACK_MESSAGE => {
                match lparam as u32 {
                    WM_LBUTTONUP => {
                        if let Err(error) = launch_status_window() {
                            show_error("Failed to open playit status", &error);
                        }
                    }
                    WM_RBUTTONUP => show_context_menu(hwnd),
                    _ => {}
                }
                return 0;
            }
            WM_DESTROY => {
                let _ = KillTimer(hwnd, POLL_TIMER_ID);
                remove_tray_icon(hwnd);
                PostQuitMessage(0);
                return 0;
            }
            WM_NCDESTROY => {
                let state = take_state(hwnd);
                if !state.is_null() {
                    drop(Box::from_raw(state));
                }
                return DefWindowProcW(hwnd, message, wparam, lparam);
            }
            _ => {}
        }

        DefWindowProcW(hwnd, message, wparam, lparam)
    }

    unsafe fn refresh_tray_visibility(hwnd: HWND) {
        let state = get_state(hwnd);
        if state.is_null() {
            return;
        }

        let state = &mut *state;
        let service_running = is_service_running();

        if service_running && !state.tray_visible {
            if add_tray_icon(hwnd, state.tray_icon) {
                state.tray_visible = true;
            }
        } else if !service_running && state.tray_visible {
            remove_tray_icon(hwnd);
            state.tray_visible = false;
        }
    }

    unsafe fn add_tray_icon(hwnd: HWND, tray_icon: HICON) -> bool {
        let mut icon_data = zeroed::<NOTIFYICONDATAW>();
        icon_data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        icon_data.hWnd = hwnd;
        icon_data.uID = TRAY_ICON_ID;
        icon_data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        icon_data.uCallbackMessage = TRAY_CALLBACK_MESSAGE;
        icon_data.hIcon = tray_icon;
        copy_wide("Playit service is running", &mut icon_data.szTip);

        Shell_NotifyIconW(NIM_ADD, &icon_data) != 0
    }

    unsafe fn remove_tray_icon(hwnd: HWND) {
        let mut icon_data = zeroed::<NOTIFYICONDATAW>();
        icon_data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        icon_data.hWnd = hwnd;
        icon_data.uID = TRAY_ICON_ID;
        let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data);
    }

    unsafe fn show_context_menu(hwnd: HWND) {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            show_error(
                "Failed to show tray menu",
                &last_error("failed to create tray menu"),
            );
            return;
        }

        let open_label = wide("Open Status");
        let stop_label = wide("Stop Service");

        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN_STATUS, open_label.as_ptr());
        let _ = AppendMenuW(menu, MF_STRING, MENU_STOP_SERVICE, stop_label.as_ptr());

        let mut cursor = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut cursor) == 0 {
            let _ = DestroyMenu(menu);
            show_error(
                "Failed to show tray menu",
                &last_error("failed to read cursor position"),
            );
            return;
        }

        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
            cursor.x,
            cursor.y,
            0,
            hwnd,
            null(),
        );
        let _ = DestroyMenu(menu);
        let _ = DefWindowProcW(hwnd, WM_NULL, 0, 0);
    }

    unsafe fn get_state(hwnd: HWND) -> *mut AppState {
        GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AppState
    }

    unsafe fn take_state(hwnd: HWND) -> *mut AppState {
        let state = get_state(hwnd);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        state
    }

    fn menu_id(wparam: WPARAM) -> usize {
        wparam & 0xFFFF
    }

    fn launch_status_window() -> Result<(), String> {
        let cli_path = playit_cli_path()?;
        Command::new(cli_path)
            .creation_flags(CREATE_NEW_CONSOLE)
            .arg("attach")
            .spawn()
            .map_err(|error| format!("Failed to launch playit.exe attach: {error}"))?;
        Ok(())
    }

    fn playit_cli_path() -> Result<PathBuf, String> {
        std::env::current_exe()
            .map(|path| path.with_file_name("playit.exe"))
            .map_err(|error| format!("Failed to resolve playit.exe path: {error}"))
    }

    unsafe fn is_service_running() -> bool {
        let manager = OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return false;
        }

        let service_name = wide(INSTALLED_SERVICE_LABEL);
        let service = OpenServiceW(manager, service_name.as_ptr(), SERVICE_QUERY_STATUS);
        if service.is_null() {
            let _ = CloseServiceHandle(manager);
            return false;
        }

        let mut status = zeroed::<SERVICE_STATUS_PROCESS>();
        let mut bytes_needed = 0;
        let running = QueryServiceStatusEx(
            service,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut bytes_needed,
        ) != 0
            && status.dwCurrentState == SERVICE_RUNNING;

        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(manager);
        running
    }

    pub fn show_error(title: &str, message: &str) {
        let title = wide(title);
        let message = wide(message);
        unsafe {
            let _ = MessageBoxW(
                null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    fn copy_wide(value: &str, buffer: &mut [u16]) {
        let encoded = value.encode_utf16();
        for (slot, value) in buffer.iter_mut().zip(encoded) {
            *slot = value;
        }
        if let Some(last) = buffer.last_mut() {
            *last = 0;
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error(context: &str) -> String {
        format!("{context} (Win32 error {})", unsafe { GetLastError() })
    }
}

#[cfg(target_os = "windows")]
fn main() {
    if let Err(error) = windows_tray::run() {
        windows_tray::show_error("Failed to start playit tray", &error);
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("playitd-tray is only supported on Windows");
    std::process::exit(1);
}
