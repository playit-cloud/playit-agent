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

use super::actions::{
    background_action_error_title, launch_playit, launch_status_window, query_service_running,
    remove_startup_shortcut,
};
use super::runtime::TrayRuntime;
use super::state::{AppEvent, AppState, BackgroundAction};
use super::util::{SingleInstanceGuard, debug_log, last_error, show_error, wide};

const INIT_TRAY_MESSAGE: u32 = WM_APP + 1;
const PROCESS_EVENTS_MESSAGE: u32 = WM_APP + 2;
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
    debug_log("registered tray window class");

    let runtime = TrayRuntime::new(event_queue.clone())?;
    let state = Box::new(AppState::new(
        event_queue.clone(),
        runtime,
        query_service_running(),
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
        (*state_ptr).runtime.set_hwnd(hwnd);
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
        PROCESS_EVENTS_MESSAGE => {
            process_pending_events(hwnd);
            return 0;
        }
        WM_TIMER => {
            if let Err(error) = dispatch_background_action(hwnd, BackgroundAction::RefreshStatus) {
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

    install_event_handlers(hwnd, state.event_queue.clone());
    state.tray = Some(tray);
    apply_service_state(state, state.service_running, state.reset_agent_enabled)?;
    dispatch_background_action(hwnd, BackgroundAction::RefreshStatus)?;

    let timer = unsafe { SetTimer(hwnd, POLL_TIMER_ID, POLL_INTERVAL_MS, None) };
    if timer == 0 {
        return Err(last_error("failed to start tray status timer"));
    }

    debug_log("tray icon initialized");

    Ok(())
}

fn install_event_handlers(hwnd: HWND, event_queue: Arc<Mutex<VecDeque<AppEvent>>>) {
    let hwnd_bits = hwnd as usize;
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
                let _ = PostMessageW(hwnd_bits as HWND, PROCESS_EVENTS_MESSAGE, 0, 0);
            }
        }
    }));

    let hwnd_bits = hwnd as usize;
    MenuEvent::set_event_handler(Some(move |event| {
        if let Ok(mut queue) = event_queue.lock() {
            queue.push_back(AppEvent::MenuActivated(event));
        }

        unsafe {
            let _ = PostMessageW(hwnd_bits as HWND, PROCESS_EVENTS_MESSAGE, 0, 0);
        }
    }));
}

fn process_pending_events(hwnd: HWND) {
    loop {
        let next_event = {
            let Some(state) = (unsafe { get_state(hwnd).as_mut() }) else {
                return;
            };

            match state.event_queue.lock() {
                Ok(mut queue) => queue.pop_front(),
                Err(_) => None,
            }
        };

        let Some(event) = next_event else {
            break;
        };

        match event {
            AppEvent::TrayClick {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
            } => {
                debug_log("tray: left click");
                if let Err(error) = launch_playit() {
                    show_error("Failed to open playit", &error);
                }
            }
            AppEvent::TrayClick {
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

                if let Err(error) =
                    dispatch_background_action(hwnd, BackgroundAction::RefreshStatus)
                {
                    show_error("Failed to refresh playit tray", &error);
                }
            }
            AppEvent::MenuActivated(menu_event) => {
                if let Err(error) = handle_menu_event(hwnd, menu_event) {
                    show_error("Playit tray action failed", &error);
                }
            }
            AppEvent::BackgroundActionFinished { action, result } => {
                if let Some(state) = unsafe { get_state(hwnd).as_mut() } {
                    state.background_busy = false;
                    if let Err(error) = apply_service_state(
                        state,
                        result.snapshot.service_running,
                        result.snapshot.reset_agent_enabled,
                    ) {
                        show_error("Failed to refresh playit tray", &error);
                    }
                }

                if let Some(error) = result.error {
                    show_error(background_action_error_title(&action), &error);
                }
            }
            AppEvent::TrayClick { .. } => {}
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
            dispatch_background_action(hwnd, BackgroundAction::StartService)?;
        }
        MenuAction::StopService => {
            debug_log("menu: stop service");
            dispatch_background_action(hwnd, BackgroundAction::StopService)?;
        }
        MenuAction::ResetAgent => {
            debug_log("menu: reset agent");
            dispatch_background_action(hwnd, BackgroundAction::ResetAgent)?;
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

fn dispatch_background_action(hwnd: HWND, action: BackgroundAction) -> Result<(), String> {
    let Some(state) = (unsafe { get_state(hwnd).as_mut() }) else {
        return Ok(());
    };

    if state.background_busy {
        return Ok(());
    }

    state.background_busy = true;
    apply_service_state(state, state.service_running, state.reset_agent_enabled)?;

    if let Err(error) = state.runtime.dispatch_background_action(action) {
        state.background_busy = false;
        apply_service_state(state, state.service_running, state.reset_agent_enabled)?;
        return Err(error);
    }

    Ok(())
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
