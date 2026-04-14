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
}

pub(super) struct AppState {
    pub(super) menu: Menu,
    pub(super) tray: Option<TrayIcon>,
    pub(super) open_status: MenuItem,
    pub(super) start_service: MenuItem,
    pub(super) stop_service: MenuItem,
    pub(super) reset_agent: MenuItem,
    pub(super) remove_tray_icon: MenuItem,
    pub(super) service_running: bool,
    pub(super) reset_agent_enabled: bool,
    pub(super) background_busy: bool,
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
    ) -> Result<Self, String> {
        let open_status = MenuItem::new("Open Status", true, None);
        let start_service = MenuItem::new("Start Service", true, None);
        let stop_service = MenuItem::new("Stop Service", true, None);
        let reset_agent = MenuItem::new("Reset Agent", true, None);
        let remove_tray_icon = MenuItem::new("Remove Tray Icon", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &open_status,
            &start_service,
            &stop_service,
            &reset_agent,
            &remove_tray_icon,
        ])
        .map_err(|error| format!("Failed to build tray menu: {error}"))?;

        Ok(Self {
            menu,
            tray: None,
            open_status,
            start_service,
            stop_service,
            reset_agent,
            remove_tray_icon,
            service_running,
            reset_agent_enabled: false,
            background_busy: false,
            menu_visible: false,
            tooltip_dirty: false,
            refresh_after_current: false,
            backend,
            response_rx,
            ui_event_queue,
        })
    }
}
