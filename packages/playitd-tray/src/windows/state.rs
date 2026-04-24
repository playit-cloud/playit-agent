use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use kanal::Receiver;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon};

use super::backend::TrayBackend;
use super::protocol::BackendResponse;

#[derive(Clone, Debug)]
pub(super) enum UiEvent {
    TrayClick {
        button: MouseButton,
        button_state: MouseButtonState,
    },
    MenuActivated(MenuEvent),
    RefreshAfterMenu,
}

pub(super) struct AppState {
    pub(super) menu: Menu,
    pub(super) tray: Option<TrayIcon>,
    pub(super) open_status: MenuItem,
    pub(super) start_service: MenuItem,
    pub(super) stop_service: MenuItem,
    pub(super) reset_agent: MenuItem,
    pub(super) add_tray_icon_to_startup: MenuItem,
    pub(super) tray_icon_action: MenuItem,
    pub(super) service_running: bool,
    pub(super) startup_shortcut_present: bool,
    pub(super) reset_agent_enabled: bool,
    pub(super) refresh_inflight: bool,
    pub(super) service_action_pending: bool,
    pub(super) menu_visible: bool,
    pub(super) tooltip_dirty: bool,
    pub(super) refresh_after_current: bool,
    pub(super) backend: TrayBackend,
    pub(super) response_rx: Receiver<BackendResponse>,
    pub(super) ui_event_queue: Arc<Mutex<VecDeque<UiEvent>>>,
}

impl AppState {
    pub(super) fn new(
        ui_event_queue: Arc<Mutex<VecDeque<UiEvent>>>,
        backend: TrayBackend,
        response_rx: Receiver<BackendResponse>,
        service_running: bool,
        startup_shortcut_present: bool,
    ) -> Result<Self, String> {
        let open_status = MenuItem::new("Open Status Window", true, None);
        let start_service = MenuItem::new("Start Background Service", true, None);
        let stop_service = MenuItem::new("Stop Background Service", true, None);
        let reset_agent = MenuItem::new("Reset Agent Setup", true, None);
        let add_tray_icon_to_startup = MenuItem::new("Show Tray Icon at Startup", true, None);
        let tray_icon_action = MenuItem::new("Close Tray Icon", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &open_status,
            &start_service,
            &stop_service,
            &reset_agent,
            &add_tray_icon_to_startup,
            &tray_icon_action,
        ])
        .map_err(|error| format!("Failed to build tray menu: {error}"))?;

        Ok(Self {
            menu,
            tray: None,
            open_status,
            start_service,
            stop_service,
            reset_agent,
            add_tray_icon_to_startup,
            tray_icon_action,
            service_running,
            startup_shortcut_present,
            reset_agent_enabled: false,
            refresh_inflight: false,
            service_action_pending: false,
            menu_visible: false,
            tooltip_dirty: false,
            refresh_after_current: false,
            backend,
            response_rx,
            ui_event_queue,
        })
    }
}
