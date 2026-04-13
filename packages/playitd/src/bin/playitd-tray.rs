#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod windows_tray {
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::fs;
    use std::mem::zeroed;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::ptr::{null, null_mut};
    use std::sync::{Arc, Mutex};

    use image::ImageFormat;
    use playitd::manager::{
        ensure_installed_service_running, stop_installed_service, INSTALLED_SERVICE_LABEL,
    };
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
    };
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT,
        SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
    };
    use windows_sys::Win32::System::Threading::{CreateMutexW, CREATE_NEW_CONSOLE};
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_CommonStartup, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        GetWindowLongPtrW, KillTimer, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
        SetTimer, SetWindowLongPtrW, TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, MB_ICONERROR,
        MB_OK, MSG, WM_APP, WM_CREATE, WM_DESTROY, WM_NCCREATE, WM_NCDESTROY, WM_TIMER, WNDCLASSW,
    };

    const INIT_TRAY_MESSAGE: u32 = WM_APP + 1;
    const PROCESS_EVENTS_MESSAGE: u32 = WM_APP + 2;
    const POLL_TIMER_ID: usize = 1;
    const POLL_INTERVAL_MS: u32 = 2_000;
    const TRAY_SHORTCUT_NAME: &str = "Playit Tray.lnk";
    const PLAYIT_ICON_BYTES: &[u8] = include_bytes!("../../../playit-cli/wix/Product.ico");

    pub fn run() -> Result<(), String> {
        let _instance_guard = match SingleInstanceGuard::new("Local\\playitd-tray")? {
            Some(guard) => guard,
            None => return Ok(()),
        };

        let event_queue = Arc::new(Mutex::new(VecDeque::new()));
        let hinstance = unsafe { GetModuleHandleW(null()) };
        if hinstance.is_null() {
            return Err(last_error("failed to get module handle"));
        }

        let class_name = wide("PlayitTrayWindowClass");
        let window_title = wide("playit tray");
        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: hinstance,
            lpszClassName: class_name.as_ptr(),
            ..unsafe { zeroed() }
        };

        if unsafe { RegisterClassW(&wnd_class) } == 0 {
            return Err(last_error("failed to register tray window class"));
        }

        let state = Box::new(AppState::new(event_queue.clone())?);
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

        unsafe {
            let _ = PostMessageW(hwnd, INIT_TRAY_MESSAGE, 0, 0);
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

            if message.message != WM_NCDESTROY {
                process_pending_events(hwnd);
            }
        }

        Ok(())
    }

    #[derive(Clone, Debug)]
    enum AppEvent {
        TrayClick {
            button: MouseButton,
            button_state: MouseButtonState,
        },
        MenuActivated(MenuEvent),
    }

    struct AppState {
        _menu: Menu,
        tray: Option<TrayIcon>,
        open_status: MenuItem,
        start_service: MenuItem,
        stop_service: MenuItem,
        remove_tray_icon: MenuItem,
        service_running: bool,
        event_queue: Arc<Mutex<VecDeque<AppEvent>>>,
    }

    impl AppState {
        fn new(event_queue: Arc<Mutex<VecDeque<AppEvent>>>) -> Result<Self, String> {
            let open_status = MenuItem::new("Open Status", true, None);
            let start_service = MenuItem::new("Start Service", true, None);
            let stop_service = MenuItem::new("Stop Service", true, None);
            let remove_tray_icon = MenuItem::new("Remove Tray Icon", true, None);

            let menu = Menu::new();
            menu.append_items(&[
                &open_status,
                &start_service,
                &stop_service,
                &remove_tray_icon,
            ])
            .map_err(|error| format!("Failed to build tray menu: {error}"))?;

            Ok(Self {
                _menu: menu,
                tray: None,
                open_status,
                start_service,
                stop_service,
                remove_tray_icon,
                service_running: query_service_running(),
                event_queue,
            })
        }
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
            WM_CREATE => return 0,
            INIT_TRAY_MESSAGE => {
                if let Err(error) = initialize_tray(hwnd) {
                    show_error("Failed to initialize playit tray", &error);
                    let _ = DestroyWindow(hwnd);
                }
                return 0;
            }
            PROCESS_EVENTS_MESSAGE => {
                process_pending_events(hwnd);
                return 0;
            }
            WM_TIMER => {
                if let Err(error) = refresh_tray_status(hwnd) {
                    show_error("Failed to refresh playit tray", &error);
                }
                return 0;
            }
            WM_DESTROY => {
                let _ = KillTimer(hwnd, POLL_TIMER_ID);
                TrayIconEvent::set_event_handler::<fn(TrayIconEvent)>(None);
                MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
                if let Some(state) = get_state(hwnd).as_mut() {
                    state.tray = None;
                }
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

    unsafe fn initialize_tray(hwnd: HWND) -> Result<(), String> {
        let Some(state) = get_state(hwnd).as_mut() else {
            return Err("tray state is missing".to_string());
        };

        let icon = load_tray_icon()?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(state._menu.clone()))
            .with_tooltip(tray_tooltip(state.service_running))
            .with_icon(icon)
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(false)
            .build()
            .map_err(|error| format!("Failed to build tray icon: {error}"))?;

        install_event_handlers(hwnd, state.event_queue.clone());
        state.tray = Some(tray);
        apply_service_state(state, state.service_running)?;

        let timer = SetTimer(hwnd, POLL_TIMER_ID, POLL_INTERVAL_MS, None);
        if timer == 0 {
            return Err(last_error("failed to start tray status timer"));
        }

        Ok(())
    }

    fn install_event_handlers(hwnd: HWND, event_queue: Arc<Mutex<VecDeque<AppEvent>>>) {
        let tray_queue = event_queue.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if let tray_icon::TrayIconEvent::Click {
                button,
                button_state,
                ..
            } = event
            {
                if let Ok(mut queue) = tray_queue.lock() {
                    queue.push_back(AppEvent::TrayClick {
                        button,
                        button_state,
                    });
                }

                unsafe {
                    let _ = PostMessageW(hwnd, PROCESS_EVENTS_MESSAGE, 0, 0);
                }
            }
        }));

        MenuEvent::set_event_handler(Some(move |event| {
            if let Ok(mut queue) = event_queue.lock() {
                queue.push_back(AppEvent::MenuActivated(event));
            }

            unsafe {
                let _ = PostMessageW(hwnd, PROCESS_EVENTS_MESSAGE, 0, 0);
            }
        }));
    }

    fn process_pending_events(hwnd: HWND) {
        let Some(state) = (unsafe { get_state(hwnd).as_mut() }) else {
            return;
        };

        loop {
            let next_event = match state.event_queue.lock() {
                Ok(mut queue) => queue.pop_front(),
                Err(_) => None,
            };

            let Some(event) = next_event else {
                break;
            };

            match event {
                AppEvent::TrayClick {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                } => {
                    if let Err(error) = launch_status_window() {
                        show_error("Failed to open playit status", &error);
                    }
                }
                AppEvent::TrayClick {
                    button: MouseButton::Right,
                    button_state: MouseButtonState::Up,
                } => {
                    if let Err(error) = refresh_tray_status(hwnd) {
                        show_error("Failed to refresh playit tray", &error);
                    }
                    if let Some(tray) = state.tray.as_ref() {
                        tray.show_menu();
                    }
                }
                AppEvent::MenuActivated(menu_event) => {
                    if let Err(error) = handle_menu_event(hwnd, state, menu_event) {
                        show_error("Playit tray action failed", &error);
                    }
                }
                _ => {}
            }
        }
    }

    fn handle_menu_event(
        hwnd: HWND,
        state: &mut AppState,
        menu_event: MenuEvent,
    ) -> Result<(), String> {
        if menu_event.id == *state.open_status.id() {
            launch_status_window()?;
            return Ok(());
        }

        if menu_event.id == *state.start_service.id() {
            start_service()?;
            return refresh_tray_status(hwnd);
        }

        if menu_event.id == *state.stop_service.id() {
            stop_installed_service()
                .map_err(|error| format!("Failed to stop playitd service: {error}"))?;
            return refresh_tray_status(hwnd);
        }

        if menu_event.id == *state.remove_tray_icon.id() {
            remove_startup_shortcut()?;
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }

        Ok(())
    }

    unsafe fn refresh_tray_status(hwnd: HWND) -> Result<(), String> {
        let Some(state) = get_state(hwnd).as_mut() else {
            return Ok(());
        };

        apply_service_state(state, query_service_running())
    }

    fn apply_service_state(state: &mut AppState, service_running: bool) -> Result<(), String> {
        state.service_running = service_running;

        state
            .start_service
            .set_enabled(!service_running)
            .map_err(|error| format!("Failed to update Start Service menu item: {error}"))?;
        state
            .stop_service
            .set_enabled(service_running)
            .map_err(|error| format!("Failed to update Stop Service menu item: {error}"))?;

        if let Some(tray) = state.tray.as_ref() {
            tray.set_tooltip(Some(tray_tooltip(service_running)))
                .map_err(|error| format!("Failed to update tray tooltip: {error}"))?;
        }

        Ok(())
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

    fn start_service() -> Result<(), String> {
        if query_service_running() {
            return Ok(());
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("Failed to create tray runtime: {error}"))?;

        runtime
            .block_on(ensure_installed_service_running())
            .map_err(|error| format!("Failed waiting for playitd service startup: {error}"))
    }

    fn playit_cli_path() -> Result<PathBuf, String> {
        std::env::current_exe()
            .map(|path| path.with_file_name("playit.exe"))
            .map_err(|error| format!("Failed to resolve playit.exe path: {error}"))
    }

    fn remove_startup_shortcut() -> Result<(), String> {
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

    fn startup_shortcut_path() -> Result<PathBuf, String> {
        unsafe {
            let mut wide_path = null_mut();
            let result = SHGetKnownFolderPath(
                &FOLDERID_CommonStartup,
                KF_FLAG_DEFAULT as u32,
                null_mut(),
                &mut wide_path,
            );

            if result < 0 {
                return Err(format!(
                    "Failed to resolve the common Startup folder (HRESULT {result:#x})"
                ));
            }

            if wide_path.is_null() {
                return Err("Common Startup folder path was empty".to_string());
            }

            let mut len = 0usize;
            while *wide_path.add(len) != 0 {
                len += 1;
            }

            let path = std::ffi::OsString::from_wide(std::slice::from_raw_parts(wide_path, len));
            CoTaskMemFree(wide_path.cast::<c_void>());

            Ok(PathBuf::from(path).join(TRAY_SHORTCUT_NAME))
        }
    }

    fn query_service_running() -> bool {
        unsafe {
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
                std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut bytes_needed,
            ) != 0
                && status.dwCurrentState == SERVICE_RUNNING;

            let _ = CloseServiceHandle(service);
            let _ = CloseServiceHandle(manager);
            running
        }
    }

    fn load_tray_icon() -> Result<Icon, String> {
        let image = image::load_from_memory_with_format(PLAYIT_ICON_BYTES, ImageFormat::Ico)
            .map_err(|error| format!("Failed to decode embedded Playit icon: {error}"))?
            .into_rgba8();
        let (width, height) = image.dimensions();

        Icon::from_rgba(image.into_raw(), width, height)
            .map_err(|error| format!("Failed to construct tray icon image: {error}"))
    }

    fn tray_tooltip(service_running: bool) -> &'static str {
        if service_running {
            "Playit service is running"
        } else {
            "Playit service is stopped"
        }
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

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error(context: &str) -> String {
        format!("{context} (Win32 error {})", unsafe { GetLastError() })
    }

    unsafe fn get_state(hwnd: HWND) -> *mut AppState {
        GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AppState
    }

    unsafe fn take_state(hwnd: HWND) -> *mut AppState {
        let state = get_state(hwnd);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        state
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
