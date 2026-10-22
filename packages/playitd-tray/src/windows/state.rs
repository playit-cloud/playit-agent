use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use kanal::Receiver;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon};

use super::backend::TrayBackend;
use super::protocol::BackendResponse;
use crate::presentation::{ServiceView, TrayPresentation};

#[derive(Clone, Debug)]
pub(super) enum UiEvent {
    TrayClick {
        button: MouseButton,
        button_state: MouseButtonState,
    },
    MenuActivated(MenuEvent),
    RefreshAfterMenu,
}

pub(super) struct TrayIcons {
    pub(super) online: Icon,
    pub(super) offline: Icon,
}

pub(super) struct MenuItems {
    pub(super) status: MenuItem,
    pub(super) open_playit: MenuItem,
    pub(super) open_dashboard: MenuItem,
    pub(super) open_logs: MenuItem,
    pub(super) start_service: MenuItem,
    pub(super) stop_service: MenuItem,
    pub(super) reset_agent: MenuItem,
    pub(super) launch_at_login: CheckMenuItem,
    pub(super) quit: MenuItem,
}

impl MenuItems {
    fn new(launch_at_login: bool) -> Self {
        Self {
            status: MenuItem::new("playit: checking status…", false, None),
            open_playit: MenuItem::new("Open playit", true, None),
            open_dashboard: MenuItem::new("Open Dashboard", true, None),
            open_logs: MenuItem::new("Open Logs Folder", true, None),
            start_service: MenuItem::new("Start Service", false, None),
            stop_service: MenuItem::new("Stop Service", false, None),
            reset_agent: MenuItem::new("Reset Agent…", false, None),
            launch_at_login: CheckMenuItem::new("Launch at Login", true, launch_at_login, None),
            quit: MenuItem::new("Quit Tray", true, None),
        }
    }
}

pub(super) struct AppState {
    pub(super) menu: Menu,
    pub(super) items: MenuItems,
    pub(super) tray: Option<TrayIcon>,
    pub(super) icons: TrayIcons,
    pub(super) view: ServiceView,
    pub(super) presentation: Option<TrayPresentation>,
    pub(super) refresh_inflight: bool,
    pub(super) refresh_after_current: bool,
    pub(super) action_pending: bool,
    pub(super) menu_visible: bool,
    pub(super) backend: TrayBackend,
    pub(super) response_rx: Receiver<BackendResponse>,
    pub(super) ui_event_queue: Arc<Mutex<VecDeque<UiEvent>>>,
}

impl AppState {
    pub(super) fn new(
        ui_event_queue: Arc<Mutex<VecDeque<UiEvent>>>,
        backend: TrayBackend,
        icons: TrayIcons,
        launch_at_login: bool,
    ) -> Result<Self, String> {
        let items = MenuItems::new(launch_at_login);
        let menu = Menu::new();
        menu.append_items(&[
            &items.status,
            &PredefinedMenuItem::separator(),
            &items.open_playit,
            &items.open_dashboard,
            &items.open_logs,
            &PredefinedMenuItem::separator(),
            &items.start_service,
            &items.stop_service,
            &items.reset_agent,
            &PredefinedMenuItem::separator(),
            &items.launch_at_login,
            &items.quit,
        ])
        .map_err(|error| format!("Failed to build tray menu: {error}"))?;

        let response_rx = backend.response_rx();
        Ok(Self {
            menu,
            items,
            tray: None,
            icons,
            view: ServiceView::Unknown("not checked yet".to_string()),
            presentation: None,
            refresh_inflight: false,
            refresh_after_current: false,
            action_pending: false,
            menu_visible: false,
            backend,
            response_rx,
            ui_event_queue,
        })
    }
}
