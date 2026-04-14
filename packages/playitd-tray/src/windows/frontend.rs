use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};

use image::ImageFormat;
use tray_icon::menu::MenuEvent;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetMessageW, GetWindowLongPtrW, KillTimer, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SetTimer, SetWindowLongPtrW, TranslateMessage, WM_APP, WM_CREATE, WM_DESTROY, WM_NCCREATE,
    WM_NCDESTROY, WM_TIMER, WNDCLASSW,
};

use super::backend::{PROCESS_BACKEND_RESPONSES_MESSAGE, TrayBackend};
use super::backend_actions::{
    launch_playit, launch_status_window, query_service_running_sync, remove_startup_shortcut,
    response_error_title,
};
use super::protocol::{BackendRequest, BackendResponse};
use super::state::{AppState, UiEvent};
use super::util::{SingleInstanceGuard, debug_log, last_error, show_error, wide};

const INIT_TRAY_MESSAGE: u32 = WM_APP + 1;
const PROCESS_UI_EVENTS_MESSAGE: u32 = WM_APP + 2;
const POLL_TIMER_ID: usize = 1;
const POLL_INTERVAL_MS: u32 = 2_000;
const PLAYIT_ICON_BYTES: &[u8] = include_bytes!("../../../playit-cli/wix/Product.ico");

pub(super) fn run() -> Result<(), String> {
    debug_log("starting tray process");

    let _instance_guard = match SingleInstanceGuard::new("Local\\playitd-tray")? {
        Some(guard) => guard,
        None => {
            debug_log("another tray instance is already running");
            return Ok(());
        }
    };

    let ui_event_queue = Arc::new(Mutex::new(VecDeque::new()));
    let backend = TrayBackend::new()?;
    let response_rx = backend.response_rx();
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
    debug_log("registered tray window class");

    let state = Box::new(AppState::new(
        ui_event_queue.clone(),
        backend,
        response_rx,
        query_service_running_sync(),
    )?);
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
    debug_log("created tray window");

    unsafe {
        (*state_ptr).backend.set_hwnd(hwnd);
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
    }

    Ok(())
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
                let state = unsafe { (*create_struct).lpCreateParams as *mut AppState };
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize as _);
                }
            }
            return 1;
        }
        WM_CREATE => return 0,
        INIT_TRAY_MESSAGE => {
            if let Err(error) = initialize_tray(hwnd) {
                show_error("Failed to initialize playit tray", &error);
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            return 0;
        }
        PROCESS_UI_EVENTS_MESSAGE => {
            process_ui_events(hwnd);
            return 0;
        }
        PROCESS_BACKEND_RESPONSES_MESSAGE => {
            process_backend_responses(hwnd);
            return 0;
        }
        WM_TIMER => {
            if let Err(error) = dispatch_request(hwnd, BackendRequest::RefreshStatus) {
                show_error("Failed to refresh playit tray", &error);
            }
            return 0;
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(hwnd, POLL_TIMER_ID);
            }
            TrayIconEvent::set_event_handler::<fn(TrayIconEvent)>(None);
            MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
            if let Some(state) = unsafe { get_state(hwnd).as_mut() } {
                state.tray = None;
                let _ = state.backend.try_send_request(BackendRequest::Shutdown);
            }
            unsafe {
                PostQuitMessage(0);
            }
            return 0;
        }
        WM_NCDESTROY => {
            let state = take_state(hwnd);
            if !state.is_null() {
                unsafe {
                    drop(Box::from_raw(state));
                }
            }
            return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
        }
        _ => {}
    }

    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn initialize_tray(hwnd: HWND) -> Result<(), String> {
    debug_log("initializing tray icon");

    let Some(state) = (unsafe { get_state(hwnd).as_mut() }) else {
        return Err("tray state is missing".to_string());
    };

    let icon = load_tray_icon()?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(state.menu.clone()))
        .with_tooltip(tray_tooltip(state.service_running))
        .with_icon(icon)
        .with_menu_on_left_click(false)
        .with_menu_on_right_click(false)
        .build()
        .map_err(|error| format!("Failed to build tray icon: {error}"))?;

    install_event_handlers(hwnd, state.ui_event_queue.clone());
    state.tray = Some(tray);
    apply_service_state(state, state.service_running, state.reset_agent_enabled)?;
    dispatch_request(hwnd, BackendRequest::RefreshStatus)?;

    let timer = unsafe { SetTimer(hwnd, POLL_TIMER_ID, POLL_INTERVAL_MS, None) };
    if timer == 0 {
        return Err(last_error("failed to start tray status timer"));
    }

    debug_log("tray icon initialized");

    Ok(())
}

fn install_event_handlers(hwnd: HWND, ui_event_queue: Arc<Mutex<VecDeque<UiEvent>>>) {
    let hwnd_bits = hwnd as usize;
    let tray_queue = ui_event_queue.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let tray_icon::TrayIconEvent::Click {
            button,
            button_state,
            ..
        } = event
        {
            if let Ok(mut queue) = tray_queue.lock() {
                queue.push_back(UiEvent::TrayClick {
                    button,
                    button_state,
                });
            }

            unsafe {
                let _ = PostMessageW(hwnd_bits as HWND, PROCESS_UI_EVENTS_MESSAGE, 0, 0);
            }
        }
    }));

    let hwnd_bits = hwnd as usize;
    MenuEvent::set_event_handler(Some(move |event| {
        if let Ok(mut queue) = ui_event_queue.lock() {
            queue.push_back(UiEvent::MenuActivated(event));
        }

        unsafe {
            let _ = PostMessageW(hwnd_bits as HWND, PROCESS_UI_EVENTS_MESSAGE, 0, 0);
        }
    }));
}

fn process_ui_events(hwnd: HWND) {
    loop {
        let next_event = {
            let Some(state) = (unsafe { get_state(hwnd).as_mut() }) else {
                return;
            };

            match state.ui_event_queue.lock() {
                Ok(mut queue) => queue.pop_front(),
                Err(_) => None,
            }
        };

        let Some(event) = next_event else {
            break;
        };

        match event {
            UiEvent::TrayClick {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
            } => {
                debug_log("tray: left click");
                if let Err(error) = launch_playit() {
                    show_error("Failed to open playit", &error);
                }
            }
            UiEvent::TrayClick {
                button: MouseButton::Right,
                button_state: MouseButtonState::Up,
            } => {
                debug_log("tray: right click");

                if let Some(state) = unsafe { get_state(hwnd).as_mut() } {
                    state.menu_visible = true;
                }

                if let Some(tray) =
                    unsafe { get_state(hwnd).as_ref() }.and_then(|state| state.tray.as_ref())
                {
                    tray.show_menu();
                }

                if let Some(state) = unsafe { get_state(hwnd).as_mut() } {
                    state.menu_visible = false;
                }

                if let Err(error) = dispatch_request(hwnd, BackendRequest::RefreshStatus) {
                    show_error("Failed to refresh playit tray", &error);
                }
            }
            UiEvent::MenuActivated(menu_event) => {
                if let Err(error) = handle_menu_event(hwnd, menu_event) {
                    show_error("Playit tray action failed", &error);
                }
            }
            UiEvent::TrayClick { .. } => {}
        }
    }
}

fn process_backend_responses(hwnd: HWND) {
    loop {
        let next_response = {
            let Some(state) = (unsafe { get_state(hwnd).as_ref() }) else {
                return;
            };

            match state.response_rx.try_recv() {
                Ok(response) => Some(response),
                Err(_) => None,
            }
        };

        let Some(response) = next_response else {
            break;
        };

        match response {
            Some(BackendResponse::RequestCompleted {
                request,
                snapshot,
                error,
            }) => {
                if let Some(state) = unsafe { get_state(hwnd).as_mut() } {
                    state.background_busy = false;
                    if let Err(apply_error) = apply_service_state(
                        state,
                        snapshot.service_running,
                        snapshot.reset_agent_enabled,
                    ) {
                        show_error("Failed to refresh playit tray", &apply_error);
                    }

                    if state.refresh_after_current {
                        state.refresh_after_current = false;
                        if let Err(dispatch_error) =
                            dispatch_request(hwnd, BackendRequest::RefreshStatus)
                        {
                            show_error("Failed to refresh playit tray", &dispatch_error);
                        }
                    }
                }

                if let Some(error) = error {
                    show_error(response_error_title(request), &error);
                }
            }
            None => break,
        }
    }
}

fn handle_menu_event(hwnd: HWND, menu_event: MenuEvent) -> Result<(), String> {
    enum MenuAction {
        OpenStatus,
        StartService,
        StopService,
        ResetAgent,
        RemoveTrayIcon,
        Ignore,
    }

    let action = {
        let Some(state) = (unsafe { get_state(hwnd).as_ref() }) else {
            return Ok(());
        };

        if menu_event.id == *state.open_status.id() {
            MenuAction::OpenStatus
        } else if menu_event.id == *state.start_service.id() {
            MenuAction::StartService
        } else if menu_event.id == *state.stop_service.id() {
            MenuAction::StopService
        } else if menu_event.id == *state.reset_agent.id() {
            MenuAction::ResetAgent
        } else if menu_event.id == *state.remove_tray_icon.id() {
            MenuAction::RemoveTrayIcon
        } else {
            MenuAction::Ignore
        }
    };

    match action {
        MenuAction::OpenStatus => {
            debug_log("menu: open status");
            launch_status_window()?;
        }
        MenuAction::StartService => {
            debug_log("menu: start service");
            dispatch_request(hwnd, BackendRequest::StartService)?;
        }
        MenuAction::StopService => {
            debug_log("menu: stop service");
            dispatch_request(hwnd, BackendRequest::StopService)?;
        }
        MenuAction::ResetAgent => {
            debug_log("menu: reset agent");
            dispatch_request(hwnd, BackendRequest::ResetAgent)?;
        }
        MenuAction::RemoveTrayIcon => {
            debug_log("menu: remove tray icon");
            remove_startup_shortcut()?;
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        MenuAction::Ignore => {}
    }

    Ok(())
}

fn apply_service_state(
    state: &mut AppState,
    service_running: bool,
    reset_agent_enabled: bool,
) -> Result<(), String> {
    let state_changed = state.service_running != service_running;
    state.service_running = service_running;
    state.reset_agent_enabled = reset_agent_enabled;

    let controls_enabled = !state.background_busy;
    state
        .start_service
        .set_enabled(!service_running && controls_enabled);
    state
        .stop_service
        .set_enabled(service_running && controls_enabled);
    state
        .reset_agent
        .set_enabled(reset_agent_enabled && controls_enabled);
    state.remove_tray_icon.set_enabled(!state.background_busy);

    if state.menu_visible {
        state.tooltip_dirty |= state_changed;
        return Ok(());
    }

    if (state_changed || state.tooltip_dirty)
        && let Some(tray) = state.tray.as_ref()
    {
        tray.set_tooltip(Some(tray_tooltip(service_running)))
            .map_err(|error| format!("Failed to update tray tooltip: {error}"))?;
        state.tooltip_dirty = false;
    }

    Ok(())
}

fn dispatch_request(hwnd: HWND, request: BackendRequest) -> Result<(), String> {
    let Some(state) = (unsafe { get_state(hwnd).as_mut() }) else {
        return Ok(());
    };

    let is_refresh = matches!(request, BackendRequest::RefreshStatus);
    if state.background_busy {
        if is_refresh {
            state.refresh_after_current = true;
        }
        return Ok(());
    }

    state.background_busy = true;
    apply_service_state(state, state.service_running, state.reset_agent_enabled)?;

    match state.backend.try_send_request(request) {
        Ok(true) => Ok(()),
        Ok(false) => {
            state.background_busy = false;
            if is_refresh {
                state.refresh_after_current = true;
            }
            apply_service_state(state, state.service_running, state.reset_agent_enabled)?;
            Ok(())
        }
        Err(error) => {
            state.background_busy = false;
            apply_service_state(state, state.service_running, state.reset_agent_enabled)?;
            Err(error)
        }
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

fn get_state(hwnd: HWND) -> *mut AppState {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AppState }
}

fn take_state(hwnd: HWND) -> *mut AppState {
    let state = get_state(hwnd);
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    }
    state
}
