use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use image::ImageFormat;
use tray_icon::menu::MenuEvent;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetMessageW, GetWindowLongPtrW, KillTimer, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SetTimer, SetWindowLongPtrW, TranslateMessage, WM_APP, WM_CREATE, WM_DESTROY, WM_NCCREATE,
    WM_NCDESTROY, WM_TIMER, WNDCLASSW,
};

use super::LaunchOptions;
use super::autostart;
use super::backend::{PROCESS_BACKEND_RESPONSES_MESSAGE, TrayBackend};
use super::backend_actions::{
    launch_playit, open_dashboard, open_logs_folder, response_error_title,
};
use super::protocol::{BackendRequest, BackendRequestKind, BackendResponse};
use super::state::{AppState, TrayIcons, UiEvent};
use super::util::{SingleInstanceGuard, confirm_warning, debug_log, last_error, show_error, wide};
use crate::presentation::TrayPresentation;

const INIT_TRAY_MESSAGE: u32 = WM_APP + 1;
const PROCESS_UI_EVENTS_MESSAGE: u32 = WM_APP + 2;
const POLL_TIMER_ID: usize = 1;
const POLL_INTERVAL_MS: u32 = 2_000;
const PLAYIT_ICON_BYTES: &[u8] = include_bytes!("../../../playit-cli/wix/Product.ico");

/// Started by the Run key or installer: failures are logged instead of shown in dialogs.
static STARTED_AT_LOGIN: AtomicBool = AtomicBool::new(false);

pub(super) fn run(options: LaunchOptions) -> Result<(), String> {
    debug_log(&format!("starting tray process {options:?}"));

    if running_in_service_session() {
        // Installers and deployment tools may start us as SYSTEM in session 0, where
        // there is no taskbar. The Run key starts the tray for each signed-in user.
        debug_log("running in session 0, exiting");
        return Ok(());
    }

    autostart::remove_legacy_startup_shortcut();

    STARTED_AT_LOGIN.store(options.autostart, Ordering::Relaxed);
    if options.autostart && !autostart::launch_at_login_enabled() {
        debug_log("launch at login is disabled for this user");
        return Ok(());
    }

    let Some(_instance_guard) = SingleInstanceGuard::new("Local\\playitd-tray")? else {
        debug_log("another tray instance is already running");
        if options.open {
            launch_playit()?;
        }
        return Ok(());
    };

    if options.open
        && let Err(error) = launch_playit()
    {
        show_error("Failed to open playit", &error);
    }

    let ui_event_queue = Arc::new(Mutex::new(VecDeque::new()));
    let backend = TrayBackend::new()?;
    let icons = load_tray_icons()?;

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

    let state = Box::new(AppState::new(
        ui_event_queue,
        backend,
        icons,
        autostart::launch_at_login_enabled(),
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
                if STARTED_AT_LOGIN.load(Ordering::Relaxed) {
                    debug_log(&format!("failed to initialize the tray: {error}"));
                } else {
                    show_error("Failed to initialize the playit tray", &error);
                }
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
            dispatch_request(hwnd, BackendRequest::RefreshStatus);
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
    let Some(state) = (unsafe { get_state(hwnd).as_mut() }) else {
        return Err("tray state is missing".to_string());
    };

    // If the taskbar is not ready yet (early sign-in), tray-icon keeps the icon and adds
    // it when Explorer broadcasts TaskbarCreated, including after Explorer restarts.
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(state.menu.clone()))
        .with_tooltip("playit")
        .with_icon(state.icons.offline.clone())
        .with_menu_on_left_click(false)
        .with_menu_on_right_click(false)
        .build()
        .map_err(|error| format!("Failed to create the tray icon: {error}"))?;

    install_event_handlers(hwnd, state.ui_event_queue.clone());
    state.tray = Some(tray);
    dispatch_request(hwnd, BackendRequest::RefreshStatus);

    if unsafe { SetTimer(hwnd, POLL_TIMER_ID, POLL_INTERVAL_MS, None) } == 0 {
        return Err(last_error("failed to start the tray status timer"));
    }

    debug_log("tray icon initialized");
    Ok(())
}

fn install_event_handlers(hwnd: HWND, ui_event_queue: Arc<Mutex<VecDeque<UiEvent>>>) {
    let hwnd_bits = hwnd as usize;
    let tray_queue = ui_event_queue.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let TrayIconEvent::Click {
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
                if let Err(error) = launch_playit() {
                    show_error("Failed to open playit", &error);
                }
            }
            UiEvent::TrayClick {
                button: MouseButton::Right,
                button_state: MouseButtonState::Up,
            } => show_tray_menu(hwnd),
            UiEvent::TrayClick { .. } => {}
            UiEvent::MenuActivated(menu_event) => {
                if let Err(error) = handle_menu_event(hwnd, menu_event) {
                    show_error("playit", &error);
                }
            }
            UiEvent::RefreshAfterMenu => dispatch_request(hwnd, BackendRequest::RefreshStatus),
        }
    }
}

fn process_backend_responses(hwnd: HWND) {
    loop {
        let Some(state) = (unsafe { get_state(hwnd).as_mut() }) else {
            return;
        };
        let Ok(Some(BackendResponse::RequestCompleted {
            request,
            view,
            error,
        })) = state.response_rx.try_recv()
        else {
            return;
        };

        match request {
            BackendRequestKind::RefreshStatus => state.refresh_inflight = false,
            _ => state.action_pending = false,
        }
        state.view = view;
        apply_view(state);

        if matches!(request, BackendRequestKind::RefreshStatus) && state.refresh_after_current {
            state.refresh_after_current = false;
            dispatch_request(hwnd, BackendRequest::RefreshStatus);
        }

        if let Some(error) = error {
            show_error(response_error_title(request), &error);
        }
    }
}

fn handle_menu_event(hwnd: HWND, menu_event: MenuEvent) -> Result<(), String> {
    enum MenuAction {
        OpenPlayit,
        OpenDashboard,
        OpenLogs,
        Request(BackendRequest),
        ResetAgent,
        LaunchAtLogin(bool),
        Quit,
    }

    // Resolve the action first so no state borrow is held while acting on it.
    let action = {
        let Some(state) = (unsafe { get_state(hwnd).as_ref() }) else {
            return Ok(());
        };
        let items = &state.items;
        let id = &menu_event.id;
        if id == items.open_playit.id() {
            MenuAction::OpenPlayit
        } else if id == items.open_dashboard.id() {
            MenuAction::OpenDashboard
        } else if id == items.open_logs.id() {
            MenuAction::OpenLogs
        } else if id == items.start_service.id() {
            MenuAction::Request(BackendRequest::StartService)
        } else if id == items.stop_service.id() {
            MenuAction::Request(BackendRequest::StopService)
        } else if id == items.reset_agent.id() {
            MenuAction::ResetAgent
        } else if id == items.launch_at_login.id() {
            MenuAction::LaunchAtLogin(items.launch_at_login.is_checked())
        } else if id == items.quit.id() {
            MenuAction::Quit
        } else {
            return Ok(());
        }
    };

    match action {
        MenuAction::OpenPlayit => launch_playit()?,
        MenuAction::OpenDashboard => open_dashboard()?,
        MenuAction::OpenLogs => open_logs_folder()?,
        MenuAction::Request(request) => dispatch_request(hwnd, request),
        MenuAction::ResetAgent => {
            if confirm_warning(
                "Reset playit agent?",
                "This disconnects this computer from your playit account and removes its secret.\n\nplayit will open so you can set it up again.",
            ) {
                dispatch_request(hwnd, BackendRequest::ResetAgent);
            }
        }
        MenuAction::LaunchAtLogin(enabled) => {
            if let Err(error) = autostart::set_launch_at_login(enabled) {
                if let Some(state) = unsafe { get_state(hwnd).as_ref() } {
                    state.items.launch_at_login.set_checked(!enabled);
                }
                return Err(error);
            }
        }
        MenuAction::Quit => unsafe {
            let _ = DestroyWindow(hwnd);
        },
    }

    Ok(())
}

fn apply_view(state: &mut AppState) {
    let presentation = TrayPresentation::from_view(&state.view);
    let items = &state.items;

    items.status.set_text(&presentation.status_line);
    items.open_playit.set_text(if presentation.needs_setup {
        "Finish Setup…"
    } else {
        "Open playit"
    });
    items
        .start_service
        .set_enabled(presentation.can_start && !state.action_pending);
    items
        .stop_service
        .set_enabled(presentation.can_stop && !state.action_pending);
    items
        .reset_agent
        .set_enabled(presentation.can_reset && !state.action_pending);

    // Changing the notification icon while its menu is open can dismiss the menu; the
    // refresh queued when the menu closes applies the change.
    if state.menu_visible || state.presentation.as_ref() == Some(&presentation) {
        return;
    }

    let Some(tray) = state.tray.as_ref() else {
        return;
    };
    let icon = if presentation.online {
        &state.icons.online
    } else {
        &state.icons.offline
    };
    let icon_result = tray.set_icon(Some(icon.clone()));
    let tooltip_result = tray.set_tooltip(Some(&presentation.tooltip));
    match (icon_result, tooltip_result) {
        // Only remember what is actually shown so the next poll retries a failed update
        // (for example while Explorer is restarting).
        (Ok(()), Ok(())) => state.presentation = Some(presentation),
        (icon_result, tooltip_result) => debug_log(&format!(
            "failed to update tray: icon={icon_result:?} tooltip={tooltip_result:?}"
        )),
    }
}

fn dispatch_request(hwnd: HWND, request: BackendRequest) {
    let Some(state) = (unsafe { get_state(hwnd).as_mut() }) else {
        return;
    };

    let is_refresh = matches!(request, BackendRequest::RefreshStatus);
    if is_refresh && state.refresh_inflight {
        state.refresh_after_current = true;
        return;
    }
    if !is_refresh && state.action_pending {
        debug_log(&format!("ignoring {request:?}: another action is running"));
        return;
    }

    match state.backend.try_send_request(request) {
        Ok(true) if is_refresh => state.refresh_inflight = true,
        Ok(true) => {
            state.action_pending = true;
            apply_view(state);
        }
        Ok(false) if is_refresh => state.refresh_after_current = true,
        Ok(false) => show_error("playit", "The tray is busy. Try again in a moment."),
        Err(error) => debug_log(&format!("failed to queue tray request: {error}")),
    }
}

fn load_tray_icons() -> Result<TrayIcons, String> {
    let image = image::load_from_memory_with_format(PLAYIT_ICON_BYTES, ImageFormat::Ico)
        .map_err(|error| format!("Failed to decode the playit icon: {error}"))?
        .into_rgba8();
    let (width, height) = image.dimensions();
    let online_rgba = image.into_raw();

    let mut offline_rgba = online_rgba.clone();
    for pixel in offline_rgba.chunks_exact_mut(4) {
        let luma =
            (u32::from(pixel[0]) * 30 + u32::from(pixel[1]) * 59 + u32::from(pixel[2]) * 11) / 100;
        let luma = luma as u8;
        pixel[0] = luma;
        pixel[1] = luma;
        pixel[2] = luma;
    }

    let icon = |rgba| {
        Icon::from_rgba(rgba, width, height)
            .map_err(|error| format!("Failed to build the tray icon: {error}"))
    };
    Ok(TrayIcons {
        online: icon(online_rgba)?,
        offline: icon(offline_rgba)?,
    })
}

fn show_tray_menu(hwnd: HWND) {
    let tray = unsafe { get_state(hwnd).as_mut() }.and_then(|state| {
        state.menu_visible = true;
        state.tray.clone()
    });
    if let Some(tray) = tray {
        // Runs a modal loop that keeps dispatching our window messages until it closes.
        tray.show_menu();
    }

    if let Some(state) = unsafe { get_state(hwnd).as_mut() } {
        state.menu_visible = false;
        if let Ok(mut queue) = state.ui_event_queue.lock() {
            queue.push_back(UiEvent::RefreshAfterMenu);
        }
    }

    unsafe {
        let _ = PostMessageW(hwnd, PROCESS_UI_EVENTS_MESSAGE, 0, 0);
    }
}

fn running_in_service_session() -> bool {
    let mut session_id = 0u32;
    let ok = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session_id) };
    ok != 0 && session_id == 0
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
