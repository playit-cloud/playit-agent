use std::time::{Duration, Instant};

use image::ImageFormat;
use playit_ipc::model::AgentLifecycle;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use super::LaunchOptions;
use super::actions::{
    Backend, Request, Response, error_title, launch_playit, open_dashboard, open_logs_folder,
    uninstall,
};
use super::login_item;
use super::util::{SingleInstance, confirm_warning, log, show_error};
use crate::presentation::{ServiceView, TrayPresentation};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const AFTER_INSTALL_SETTLE_TIMEOUT: Duration = Duration::from_secs(15);
const PLAYIT_ICON_BYTES: &[u8] = include_bytes!("../../../playit-cli/wix/Product.ico");

#[derive(Debug)]
pub(super) enum UserEvent {
    Menu(MenuEvent),
    Backend(Response),
}

struct MenuItems {
    status: MenuItem,
    open_playit: MenuItem,
    open_dashboard: MenuItem,
    open_logs: MenuItem,
    start_service: MenuItem,
    stop_service: MenuItem,
    reset_agent: MenuItem,
    launch_at_login: CheckMenuItem,
    uninstall: MenuItem,
    quit: MenuItem,
}

impl MenuItems {
    fn new() -> Self {
        Self {
            status: MenuItem::new("playit: checking status…", false, None),
            open_playit: MenuItem::new("Open playit", true, None),
            open_dashboard: MenuItem::new("Open Dashboard", true, None),
            open_logs: MenuItem::new("Open Logs Folder", true, None),
            start_service: MenuItem::new("Start Service", false, None),
            stop_service: MenuItem::new("Stop Service", false, None),
            reset_agent: MenuItem::new("Reset Agent…", false, None),
            launch_at_login: CheckMenuItem::new("Open at Login", true, login_item::enabled(), None),
            uninstall: MenuItem::new("Uninstall playit…", true, None),
            quit: MenuItem::new("Quit Menu Bar App", true, None),
        }
    }

    fn menu(&self) -> Result<Menu, String> {
        let menu = Menu::new();
        menu.append_items(&[
            &self.status,
            &PredefinedMenuItem::separator(),
            &self.open_playit,
            &self.open_dashboard,
            &self.open_logs,
            &PredefinedMenuItem::separator(),
            &self.start_service,
            &self.stop_service,
            &self.reset_agent,
            &PredefinedMenuItem::separator(),
            &self.launch_at_login,
            &self.uninstall,
            &self.quit,
        ])
        .map_err(|error| format!("Failed to build the menu: {error}"))?;
        Ok(menu)
    }
}

struct TrayIcons {
    online: Icon,
    offline: Icon,
}

struct AppState {
    items: MenuItems,
    tray: TrayIcon,
    icons: TrayIcons,
    backend: Backend,
    presentation: Option<TrayPresentation>,
    refresh_inflight: bool,
    refresh_after_current: bool,
    action_pending: bool,
    /// After an install, the first settled status decides whether to open setup. An upgrade
    /// restarts the service, so it reports Starting for a moment first.
    after_install_deadline: Option<Instant>,
}

pub(super) fn run(options: LaunchOptions) -> Result<(), String> {
    if options.autostart && !login_item::enabled() {
        return Ok(());
    }

    let Some(instance) = SingleInstance::acquire()? else {
        // Finder reopens a running app instead of starting it, so this is a second login item
        // or a direct launch.
        if !options.autostart && !options.after_install {
            launch_playit()?;
        }
        return Ok(());
    };

    if let Err(error) = login_item::sync() {
        log(&format!("failed to update the login item: {error}"));
    }

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));
    let backend = Backend::spawn(proxy)?;
    let icons = load_tray_icons()?;

    let mut setup = Some((backend, icons));
    let mut state: Option<AppState> = None;
    event_loop.run(move |event, _, control_flow| {
        let _instance: &SingleInstance = &instance;
        match event {
            // The status item must be created once the app has finished launching.
            Event::NewEvents(StartCause::Init) => {
                let Some((backend, icons)) = setup.take() else {
                    return;
                };
                match AppState::new(options, backend, icons) {
                    Ok(mut app) => {
                        app.dispatch(Request::RefreshStatus);
                        if !options.autostart
                            && !options.after_install
                            && let Err(error) = launch_playit()
                        {
                            show_error("Failed to open playit", &error);
                        }
                        state = Some(app);
                    }
                    Err(error) => {
                        if options.autostart {
                            log(&error);
                        } else {
                            show_error("Failed to start playit", &error);
                        }
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                }
                *control_flow = ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL);
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if let Some(app) = state.as_mut() {
                    app.dispatch(Request::RefreshStatus);
                }
                *control_flow = ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL);
            }
            Event::UserEvent(UserEvent::Backend(response)) => {
                if let Some(app) = state.as_mut() {
                    app.handle_response(response);
                }
            }
            Event::UserEvent(UserEvent::Menu(menu_event)) => {
                if let Some(app) = state.as_mut()
                    && app.handle_menu_event(menu_event)
                {
                    *control_flow = ControlFlow::Exit;
                }
            }
            // Opening playit.app while it is already running.
            Event::Reopen { .. } => {
                if let Err(error) = launch_playit() {
                    show_error("Failed to open playit", &error);
                }
            }
            _ => {}
        }
    })
}

impl AppState {
    fn new(options: LaunchOptions, backend: Backend, icons: TrayIcons) -> Result<Self, String> {
        let items = MenuItems::new();
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(items.menu()?))
            .with_tooltip("playit")
            .with_icon(icons.offline.clone())
            .build()
            .map_err(|error| format!("Failed to create the menu bar icon: {error}"))?;

        Ok(Self {
            items,
            tray,
            icons,
            backend,
            presentation: None,
            refresh_inflight: false,
            refresh_after_current: false,
            action_pending: false,
            after_install_deadline: options
                .after_install
                .then(|| Instant::now() + AFTER_INSTALL_SETTLE_TIMEOUT),
        })
    }

    fn dispatch(&mut self, request: Request) {
        let is_refresh = matches!(request, Request::RefreshStatus);
        if is_refresh && self.refresh_inflight {
            self.refresh_after_current = true;
            return;
        }
        if !is_refresh && self.action_pending {
            log(&format!("ignoring {request:?}: another action is running"));
            return;
        }

        match (self.backend.try_send(request), is_refresh) {
            (true, true) => self.refresh_inflight = true,
            (true, false) => {
                self.action_pending = true;
                self.presentation = None;
            }
            (false, true) => self.refresh_after_current = true,
            (false, false) => show_error("playit", "playit is busy. Try again in a moment."),
        }
    }

    fn handle_response(&mut self, response: Response) {
        match response.request {
            Request::RefreshStatus => self.refresh_inflight = false,
            _ => self.action_pending = false,
        }

        if let Some(deadline) = self.after_install_deadline
            && (is_settled(&response.view) || Instant::now() >= deadline)
        {
            self.after_install_deadline = None;
            if needs_setup(&response.view)
                && let Err(error) = launch_playit()
            {
                log(&format!("failed to open playit after install: {error}"));
            }
        }

        self.apply_view(&response.view);

        if matches!(response.request, Request::RefreshStatus) && self.refresh_after_current {
            self.refresh_after_current = false;
            self.dispatch(Request::RefreshStatus);
        }

        if let Some(error) = response.error {
            show_error(error_title(response.request), &error);
        }
    }

    /// Returns true when the app should quit.
    fn handle_menu_event(&mut self, event: MenuEvent) -> bool {
        let id = &event.id;
        let items = &self.items;
        let result = if id == items.open_playit.id() {
            launch_playit()
        } else if id == items.open_dashboard.id() {
            open_dashboard()
        } else if id == items.open_logs.id() {
            open_logs_folder()
        } else if id == items.start_service.id() {
            self.dispatch(Request::StartService);
            Ok(())
        } else if id == items.stop_service.id() {
            self.dispatch(Request::StopService);
            Ok(())
        } else if id == items.reset_agent.id() {
            if confirm_warning(
                "Reset playit agent?",
                "Resetting disconnects all tunnels on this Mac and removes its saved account connection.\n\nplayit will open for setup. Complete setup again before tunnels can reconnect.",
                "Reset",
            ) {
                self.dispatch(Request::ResetAgent);
            }
            Ok(())
        } else if id == items.launch_at_login.id() {
            let enabled = items.launch_at_login.is_checked();
            login_item::set_enabled(enabled).inspect_err(|_| {
                items.launch_at_login.set_checked(!enabled);
            })
        } else if id == items.uninstall.id() {
            return self.confirm_and_uninstall();
        } else if id == items.quit.id() {
            return true;
        } else {
            Ok(())
        };

        if let Err(error) = result {
            show_error("playit", &error);
        }
        false
    }

    fn confirm_and_uninstall(&self) -> bool {
        if !confirm_warning(
            "Uninstall playit?",
            "This stops the playit service, removes it from login items, and deletes playit.app for all users on this Mac.\n\nYour agent's secret key stays in ~/Library/Application Support/playit_gg, so reinstalling reconnects the same agent.",
            "Uninstall",
        ) {
            return false;
        }
        match uninstall() {
            Ok(()) => true,
            Err(error) if error.is_empty() => false,
            Err(error) => {
                show_error("Failed to uninstall playit", &error);
                false
            }
        }
    }

    fn apply_view(&mut self, view: &ServiceView) {
        let presentation = TrayPresentation::from_view(view);
        let items = &self.items;
        let idle = !self.action_pending;

        items.status.set_text(&presentation.status_line);
        items.open_playit.set_text(if presentation.needs_setup {
            "Finish Setup…"
        } else {
            "Open playit"
        });
        // Starting installs the LaunchAgent when it is missing.
        let can_start = presentation.can_start || matches!(view, ServiceView::NotInstalled);
        items.start_service.set_enabled(can_start && idle);
        items
            .stop_service
            .set_enabled(presentation.can_stop && idle);
        items
            .reset_agent
            .set_enabled(presentation.can_reset && idle);

        if self.presentation.as_ref() == Some(&presentation) {
            return;
        }

        let icon = if presentation.online {
            &self.icons.online
        } else {
            &self.icons.offline
        };
        let icon_result = self.tray.set_icon(Some(icon.clone()));
        let tooltip_result = self.tray.set_tooltip(Some(&presentation.tooltip));
        match (icon_result, tooltip_result) {
            (Ok(()), Ok(())) => {
                log(&format!("status: {}", presentation.status_line));
                self.presentation = Some(presentation);
            }
            (icon_result, tooltip_result) => log(&format!(
                "failed to update the menu bar icon: icon={icon_result:?} tooltip={tooltip_result:?}"
            )),
        }
    }
}

fn is_settled(view: &ServiceView) -> bool {
    !matches!(
        view,
        ServiceView::Starting
            | ServiceView::Stopping
            | ServiceView::Unknown(_)
            | ServiceView::Agent(AgentLifecycle::Starting | AgentLifecycle::Stopping)
    )
}

fn needs_setup(view: &ServiceView) -> bool {
    matches!(
        view,
        ServiceView::NotInstalled
            | ServiceView::Stopped
            | ServiceView::Agent(AgentLifecycle::WaitingForSecret)
    )
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
            .map_err(|error| format!("Failed to build the menu bar icon: {error}"))
    };
    Ok(TrayIcons {
        online: icon(online_rgba)?,
        offline: icon(offline_rgba)?,
    })
}
