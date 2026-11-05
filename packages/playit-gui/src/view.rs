use std::time::{Duration, Instant};

use chrono::Local;
use gpui::{
    AnyElement, ClipboardItem, Context, Div, ElementId, FontWeight, Hsla, ScrollStrategy,
    SharedString, Stateful, Svg, Task, UniformListScrollHandle, Window, div, prelude::*, px, rgb,
    svg, uniform_list,
};
use playit_ipc::model::{AccountStatus, AgentState, LogEntry, LogLevel};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::assets::Icon;
use crate::backend::{BackendEvent, CommandOutcome, SetupStep, UiCommand};
use crate::state::{
    ACCOUNT_AGENTS_URL, ACCOUNT_UPGRADE_URL, AppState, Screen, SessionMode, disabled_reason,
    format_bytes, format_uptime, level_label, log_time_label, short_target,
};

mod theme {
    use gpui::{Hsla, rgb};

    pub fn bg() -> Hsla {
        rgb(0x111118).into()
    }
    pub fn panel() -> Hsla {
        rgb(0x1a1a24).into()
    }
    pub fn panel_raised() -> Hsla {
        rgb(0x242433).into()
    }
    pub fn inset() -> Hsla {
        rgb(0x13131b).into()
    }
    pub fn border() -> Hsla {
        rgb(0x2e2e40).into()
    }
    pub fn border_strong() -> Hsla {
        rgb(0x3d3d52).into()
    }
    pub fn text() -> Hsla {
        rgb(0xececf3).into()
    }
    pub fn muted() -> Hsla {
        rgb(0xa9a9be).into()
    }
    pub fn faint() -> Hsla {
        rgb(0x7c7c94).into()
    }
    pub fn accent() -> Hsla {
        rgb(0xd17bf7).into()
    }
    pub fn accent_strong() -> Hsla {
        rgb(0x9d4edd).into()
    }
    pub fn accent_hover() -> Hsla {
        rgb(0xb064f0).into()
    }
    pub fn green() -> Hsla {
        rgb(0x4ade80).into()
    }
    pub fn yellow() -> Hsla {
        rgb(0xfacc15).into()
    }
    pub fn red() -> Hsla {
        rgb(0xf87171).into()
    }
    pub fn blue() -> Hsla {
        rgb(0x60a5fa).into()
    }
    pub fn grey() -> Hsla {
        rgb(0x8b8ba1).into()
    }
}

#[cfg(target_os = "macos")]
const MONO_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
const MONO_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO_FONT: &str = "DejaVu Sans Mono";

const LOG_ROW_HEIGHT: f32 = 20.;
/// Below this many visible lines the log panel shrinks to fit instead of filling the window.
const LOG_FIT_ROWS: usize = 10;
const TOAST_DURATION: Duration = Duration::from_secs(2);

pub struct PlayitWindow {
    state: AppState,
    commands: UnboundedSender<UiCommand>,
    log_scroll: UniformListScrollHandle,
    follow_logs: bool,
    show_debug_logs: bool,
    /// `None` follows the default for the screen: open while running, closed otherwise.
    logs_expanded: Option<bool>,
    toast: Option<(SharedString, Instant)>,
    command_error: Option<String>,
    _tasks: Vec<Task<()>>,
}

impl PlayitWindow {
    pub fn new(
        state: AppState,
        commands: UnboundedSender<UiCommand>,
        mut events: UnboundedReceiver<BackendEvent>,
        mut outcomes: UnboundedReceiver<CommandOutcome>,
        cx: &mut Context<Self>,
    ) -> Self {
        let events_task = cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                // Logs arrive in bursts, so render once per batch rather than once per line.
                let mut batch = vec![event];
                while let Ok(event) = events.try_recv() {
                    batch.push(event);
                }
                let updated = this.update(cx, |view, cx| {
                    for event in batch {
                        view.state.apply(event);
                    }
                    view.after_state_change();
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });

        let outcomes_task = cx.spawn(async move |this, cx| {
            while let Some(outcome) = outcomes.recv().await {
                let updated = this.update(cx, |view, cx| {
                    match outcome {
                        CommandOutcome::OpenUrl(url) => cx.open_url(&url),
                        CommandOutcome::Failed(error) => view.command_error = Some(error),
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });

        // Keeps the uptime counter moving and clears expired toasts.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let updated = this.update(cx, |view, cx| {
                    if view
                        .toast
                        .as_ref()
                        .is_some_and(|(_, shown)| shown.elapsed() >= TOAST_DURATION)
                    {
                        view.toast = None;
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });

        Self {
            state,
            commands,
            log_scroll: UniformListScrollHandle::new(),
            follow_logs: true,
            show_debug_logs: false,
            logs_expanded: None,
            toast: None,
            command_error: None,
            _tasks: vec![events_task, outcomes_task, ticker],
        }
    }

    fn after_state_change(&mut self) {
        if self.state.needs_setup() {
            self.state.begin_setup();
            self.send(UiCommand::StartSetup);
        }
        let visible = self.visible_log_count();
        if self.follow_logs && visible > 0 {
            self.log_scroll
                .scroll_to_item(visible - 1, ScrollStrategy::Bottom);
        }
    }

    fn send(&mut self, command: UiCommand) {
        if self.commands.send(command).is_err() {
            self.command_error = Some("The playit background worker stopped.".to_string());
        }
    }

    fn copy(&mut self, text: String, label: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.toast = Some((format!("Copied {label}").into(), Instant::now()));
        cx.notify();
    }

    fn shows_log(&self, entry: &LogEntry) -> bool {
        self.show_debug_logs || !matches!(entry.level, LogLevel::Debug | LogLevel::Trace)
    }

    fn visible_log_count(&self) -> usize {
        self.state
            .logs
            .iter()
            .filter(|entry| self.shows_log(entry))
            .count()
    }

    fn render_header(&self, screen: &Screen, cx: &mut Context<Self>) -> impl IntoElement {
        let (status_label, status_color) = match screen {
            Screen::Running(_) => ("Online", theme::green()),
            Screen::Connecting => ("Connecting", theme::yellow()),
            Screen::Starting => ("Starting", theme::yellow()),
            Screen::Stopping => ("Stopping", theme::yellow()),
            Screen::Setup(_) => ("Not set up", theme::blue()),
            Screen::Unreachable { .. } => ("Offline", theme::red()),
            Screen::InvalidSecret { .. } => ("Sign-in problem", theme::red()),
            Screen::OverLimit => ("Agent limit reached", theme::red()),
            Screen::Error { .. } => ("Error", theme::red()),
        };

        let mut right = div().flex().items_center().gap_4();
        if let Screen::Running(agent) = screen {
            right = right
                .child(header_stat(
                    "Uptime",
                    div()
                        .text_color(theme::text())
                        .child(format_uptime(agent.start_time, now_millis()))
                        .into_any_element(),
                ))
                .child(header_stat(
                    "Account",
                    self.render_account(&agent.account_status, cx),
                ))
                .child(
                    button(
                        "dashboard",
                        "Dashboard",
                        ButtonStyle::Secondary,
                        Some(Icon::External),
                    )
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.send(UiCommand::OpenDashboard);
                        cx.notify();
                    })),
                );
        }

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_5()
            .h(px(64.))
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::accent())
                            .child("playit"),
                    )
                    .child(pill(status_label, status_color)),
            )
            .child(right)
    }

    fn render_account(&self, status: &AccountStatus, cx: &mut Context<Self>) -> AnyElement {
        let row = div().flex().items_center().gap_1p5();
        match status {
            AccountStatus::Verified => row
                .text_color(theme::green())
                .child(icon(Icon::CheckCircle, 14., theme::green()))
                .child("Verified")
                .into_any_element(),
            AccountStatus::EmailNotVerified => row
                .text_color(theme::yellow())
                .child(icon(Icon::Alert, 14., theme::yellow()))
                .child("Email not verified")
                .into_any_element(),
            AccountStatus::Guest => row
                .text_color(theme::muted())
                .child(icon(Icon::User, 14., theme::muted()))
                .child("Guest")
                .child(
                    div()
                        .id("claim-account")
                        .text_color(theme::accent())
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .child("Save account")
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.send(UiCommand::OpenDashboard);
                            cx.notify();
                        })),
                )
                .into_any_element(),
            AccountStatus::Unknown => row
                .text_color(theme::muted())
                .child("Unknown")
                .into_any_element(),
        }
    }

    fn render_running(&self, agent: &AgentState, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col().gap_4().flex_1();

        for (ix, notice) in agent.notices.iter().enumerate() {
            let color = match notice.priority.to_ascii_lowercase().as_str() {
                "high" | "critical" | "error" => theme::red(),
                "low" | "info" => theme::blue(),
                _ => theme::yellow(),
            };
            let mut banner = div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(color.opacity(0.45))
                .bg(color.opacity(0.08))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(icon(Icon::Info, 16., color))
                        .child(div().min_w_0().child(notice.message.clone())),
                );
            if let Some(link) = notice.resolve_link.clone() {
                banner = banner.child(
                    button(
                        ("notice", ix),
                        "Learn more",
                        ButtonStyle::Secondary,
                        Some(Icon::External),
                    )
                    .on_click(move |_, _, cx| cx.open_url(&link)),
                );
            }
            body = body.child(banner);
        }

        body.child(self.render_tunnels(agent, cx))
            .child(self.render_stats())
            .child(self.render_logs(true, cx))
    }

    fn render_tunnels(&self, agent: &AgentState, cx: &mut Context<Self>) -> impl IntoElement {
        let online = agent.tunnels.iter().filter(|t| !t.is_disabled).count();
        let disabled = agent.tunnels.len() - online;
        let mut summary = vec![format!("{online} online")];
        if disabled > 0 {
            summary.push(format!("{disabled} disabled"));
        }
        if !agent.pending_tunnels.is_empty() {
            summary.push(format!("{} setting up", agent.pending_tunnels.len()));
        }

        let mut list = div()
            .id("tunnel-list")
            .flex()
            .flex_col()
            .p_2()
            .max_h(px(300.))
            .overflow_y_scroll();

        if agent.tunnels.is_empty() && agent.pending_tunnels.is_empty() {
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .py_6()
                    .px_4()
                    .child(
                        div()
                            .size(px(40.))
                            .flex_none()
                    .min_h(px(40.))
                    .min_w(px(40.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme::accent().opacity(0.12))
                            .text_color(theme::accent())
                            .child(icon(Icon::Plus, 20., theme::accent())),
                    )
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("No tunnels yet"),
                    )
                    .child(
                        div()
                            .text_color(theme::muted())
                            .text_center()
                            .child("Create a tunnel on playit.gg to share a game server. It will show up here automatically."),
                    )
                    .child(
                        large(button(
                            "add-tunnel",
                            "Create a tunnel",
                            ButtonStyle::Primary,
                            Some(Icon::External),
                        ))
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.send(UiCommand::OpenDashboard);
                            cx.notify();
                        })),
                    ),
            );
        }

        for (ix, tunnel) in agent.tunnels.iter().enumerate() {
            let (dot_color, chip_label, chip_color) = if tunnel.is_disabled {
                (theme::grey(), "Disabled", theme::grey())
            } else {
                (theme::green(), "Online", theme::green())
            };
            let detail = if tunnel.is_disabled {
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::yellow())
                    .child(disabled_reason(tunnel.disabled_reason.as_deref()))
            } else {
                div()
                    .flex()
                    .gap_1p5()
                    .min_w_0()
                    .child(div().flex_none().text_color(theme::faint()).child("Local"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO_FONT)
                            .text_color(theme::muted())
                            .child(tunnel.destination.clone()),
                    )
            };

            let mut actions = div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .child(chip(chip_label, chip_color));
            if !tunnel.is_disabled {
                let address = tunnel.display_address.clone();
                actions =
                    actions.child(
                        button(
                            ("copy-tunnel", ix),
                            "Copy",
                            ButtonStyle::Secondary,
                            Some(Icon::Copy),
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.copy(address.clone(), "address", cx)
                        })),
                    );
            }

            list = list.child(tunnel_row(
                ("tunnel", ix),
                dot_color,
                tunnel.display_address.clone(),
                !tunnel.is_disabled,
                detail,
                actions,
            ));
        }

        for (ix, pending) in agent.pending_tunnels.iter().enumerate() {
            list = list.child(tunnel_row(
                ("pending", ix),
                theme::yellow(),
                pending.id.clone(),
                false,
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::muted())
                    .child(format!("{}…", sentence_case(&pending.status_msg))),
                div()
                    .flex()
                    .flex_none()
                    .child(chip("Setting up", theme::yellow())),
            ));
        }

        panel()
            .flex()
            .flex_col()
            .flex_none()
            .child(panel_header(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Tunnels")
                    .into_any_element(),
                (!agent.tunnels.is_empty() || !agent.pending_tunnels.is_empty()).then(|| {
                    div()
                        .text_xs()
                        .text_color(theme::muted())
                        .child(summary.join(" · "))
                        .into_any_element()
                }),
            ))
            .child(list)
    }

    fn render_stats(&self) -> impl IntoElement {
        let stats = &self.state.stats;
        panel()
            .flex_none()
            .flex()
            .child(stat_cell(
                Icon::ArrowDown,
                "Received",
                format_bytes(stats.bytes_in),
                true,
            ))
            .child(stat_cell(
                Icon::ArrowUp,
                "Sent",
                format_bytes(stats.bytes_out),
                true,
            ))
            .child(stat_cell(
                Icon::Plug,
                "TCP connections",
                stats.active_tcp.to_string(),
                true,
            ))
            .child(stat_cell(
                Icon::Activity,
                "UDP sessions",
                stats.active_udp.to_string(),
                false,
            ))
    }

    fn render_logs(&self, default_expanded: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let expanded = self.logs_expanded.unwrap_or(default_expanded);
        let visible = self.visible_log_count();
        let total = self.state.logs.len();

        let title = div()
            .id("toggle-logs")
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .child(icon(
                if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                16.,
                theme::muted(),
            ))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Activity log"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::faint())
                    .child(format!("{total} messages")),
            )
            .on_click(cx.listener(move |view, _, _, cx| {
                view.logs_expanded = Some(!expanded);
                view.after_state_change();
                cx.notify();
            }));

        let controls = expanded.then(|| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    toggle("follow-logs", "Auto-scroll", self.follow_logs).on_click(cx.listener(
                        |view, _, _, cx| {
                            view.follow_logs = !view.follow_logs;
                            view.after_state_change();
                            cx.notify();
                        },
                    )),
                )
                .child(
                    toggle("debug-logs", "Debug messages", self.show_debug_logs).on_click(
                        cx.listener(|view, _, _, cx| {
                            view.show_debug_logs = !view.show_debug_logs;
                            view.after_state_change();
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    button(
                        "copy-logs",
                        "Copy",
                        ButtonStyle::Secondary,
                        Some(Icon::Copy),
                    )
                    .on_click(cx.listener(|view, _, _, cx| {
                        let text = view.state.logs_text();
                        view.copy(text, "activity log", cx)
                    })),
                )
                .into_any_element()
        });

        let mut panel = panel()
            .flex()
            .flex_col()
            .child(panel_header(title.into_any_element(), controls));

        if !expanded {
            return panel.flex_none();
        }

        if visible == 0 {
            return panel.flex_none().child(
                div()
                    .px_4()
                    .py_3()
                    .text_color(theme::faint())
                    .child("Nothing to show yet."),
            );
        }

        let list = uniform_list(
            "logs",
            visible,
            cx.processor(|view, range: std::ops::Range<usize>, _window, _cx| {
                let today = Local::now().date_naive();
                view.state
                    .logs
                    .iter()
                    .filter(|entry| view.shows_log(entry))
                    .skip(range.start)
                    .take(range.len())
                    .map(|entry| log_row(entry, today))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.log_scroll.clone());

        let list_area = div()
            .flex()
            .flex_col()
            .py_1()
            .font_family(MONO_FONT)
            .text_xs();
        if visible <= LOG_FIT_ROWS {
            panel = panel
                .flex_none()
                .child(list_area.child(list.h(px(visible as f32 * LOG_ROW_HEIGHT))));
        } else {
            panel = panel
                .flex_1()
                .min_h(px(180.))
                .child(list_area.flex_1().min_h_0().child(list.flex_1()));
        }
        panel
    }

    fn render_message(&self, screen: &Screen, cx: &mut Context<Self>) -> Div {
        let mut card = MessageCard::default();

        match screen {
            Screen::Connecting => {
                card.icon = Icon::Clock;
                card.color = theme::yellow();
                card.title = "Connecting to playit".into();
                card.body =
                    vec!["Looking for the playit background service on this computer…".into()];
            }
            Screen::Unreachable { error } => {
                card.icon = Icon::Power;
                card.color = theme::red();
                card.title = "playit isn't running".into();
                let guidance = unreachable_guidance(self.state.session_mode, error);
                card.body = guidance.body;
                card.command = guidance.command;
                card.details = Some(format!("Address: {}\n{}", self.state.socket_path, error));
                card.waiting = Some("Retrying automatically every few seconds…");
            }
            Screen::Starting => {
                card.icon = Icon::Clock;
                card.color = theme::yellow();
                card.title = "Starting playit".into();
                card.body = vec![
                    "Connecting to the playit network. This usually takes a few seconds.".into(),
                ];
            }
            Screen::Stopping => {
                card.icon = Icon::Clock;
                card.color = theme::yellow();
                card.title = "playit is stopping".into();
                card.body = vec!["Your tunnels are going offline.".into()];
            }
            Screen::Error { message } => {
                card.icon = Icon::Alert;
                card.color = theme::red();
                card.title = "playit hit a problem".into();
                card.body = vec![
                    "The agent stopped because of an error and will keep retrying. If this keeps happening, copy the details below and share them with playit support.".into(),
                ];
                card.details = Some(message.to_string());
                card.actions.push(
                    large(button(
                        "error-dashboard",
                        "Open dashboard",
                        ButtonStyle::Primary,
                        Some(Icon::External),
                    ))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.send(UiCommand::OpenDashboard);
                        cx.notify();
                    }))
                    .into_any_element(),
                );
            }
            Screen::InvalidSecret { message } => {
                card.icon = Icon::Link;
                card.color = theme::red();
                card.title = "Link this computer again".into();
                card.body = vec![
                    "playit couldn't sign in with the key saved on this computer. The agent may have been removed from your account.".into(),
                    "Linking again takes a few seconds in your browser.".into(),
                ];
                card.details = Some(message.to_string());
                card.actions.push(
                    large(button(
                        "reset-secret",
                        "Link again",
                        ButtonStyle::Primary,
                        Some(Icon::Link),
                    ))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.command_error = None;
                        view.send(UiCommand::ResetSecret);
                        cx.notify();
                    }))
                    .into_any_element(),
                );
            }
            Screen::OverLimit => {
                card.icon = Icon::Alert;
                card.color = theme::red();
                card.title = "Your account has too many agents".into();
                card.body = vec![
                    "Each computer running playit uses one agent slot. Remove an agent you no longer use, or upgrade your plan, and playit will start automatically.".into(),
                ];
                card.actions.push(link_button(
                    "agents",
                    "Manage agents",
                    ACCOUNT_AGENTS_URL,
                    ButtonStyle::Primary,
                ));
                card.actions.push(link_button(
                    "upgrade",
                    "Upgrade plan",
                    ACCOUNT_UPGRADE_URL,
                    ButtonStyle::Secondary,
                ));
            }
            Screen::Setup(setup) => {
                card.icon = Icon::Link;
                card.color = theme::accent();
                card.title = "Connect playit to your account".into();
                card.body = vec![
                    "Link this computer to your playit account to start sharing your servers."
                        .into(),
                ];
                match &setup.step {
                    SetupStep::Rejected | SetupStep::Failed(_) => {
                        card.color = theme::red();
                        card.body = vec![match &setup.step {
                            SetupStep::Failed(error) => {
                                format!("Setup didn't finish: {error}")
                            }
                            _ => "Setup was declined in the browser.".to_string(),
                        }];
                        card.actions.push(
                            large(button(
                                "retry-setup",
                                "Try again",
                                ButtonStyle::Primary,
                                None,
                            ))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.state.begin_setup();
                                view.send(UiCommand::StartSetup);
                                cx.notify();
                            }))
                            .into_any_element(),
                        );
                    }
                    step => {
                        card.steps = Some(setup_progress(step));
                        card.waiting = Some(match step {
                            SetupStep::Preparing => "Preparing your setup link…",
                            SetupStep::Approved => "Approved. Finishing setup…",
                            _ => "Waiting for you to finish in your browser…",
                        });
                        if let Some(url) = setup.url.clone() {
                            let copy_url = url.clone();
                            card.actions.push(link_button(
                                "open-setup",
                                "Open setup page",
                                &url,
                                ButtonStyle::Primary,
                            ));
                            card.actions.push(
                                large(button(
                                    "copy-setup",
                                    "Copy link",
                                    ButtonStyle::Secondary,
                                    Some(Icon::Copy),
                                ))
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.copy(copy_url.clone(), "link", cx)
                                }))
                                .into_any_element(),
                            );
                            card.link = Some(url);
                        }
                    }
                }
            }
            Screen::Running(_) => unreachable!("running screen has its own layout"),
        }

        let mut body = div().flex().flex_col().gap_4().flex_1().child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .py_4()
                .child(card.render(cx)),
        );
        if !self.state.logs.is_empty() {
            body = body.child(self.render_logs(false, cx));
        }
        body
    }

    fn render_footer(&self, screen: &Screen, cx: &mut Context<Self>) -> impl IntoElement {
        let message = match (screen, self.state.session_mode) {
            (Screen::Unreachable { .. }, _) => "Not connected to the playit background service.",
            (Screen::Connecting, _) => "Connecting to the playit background service…",
            (_, SessionMode::Service) => {
                "playit keeps running in the background when you close this window."
            }
            (_, SessionMode::ExternalDaemon) => "Closing this window won't stop the playit agent.",
        };

        let mut right = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .text_color(theme::faint());
        if let Screen::Running(agent) = screen
            && !agent.agent_id.is_empty()
        {
            let agent_id = agent.agent_id.clone();
            let short_id: String = agent.agent_id.chars().take(8).collect();
            right = right.child(
                div()
                    .id("agent-id")
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme::muted()))
                    .child(format!("Agent {short_id}"))
                    .child(icon(Icon::Copy, 12., theme::faint()))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.copy(agent_id.clone(), "agent ID", cx)
                    })),
            );
        }
        let version = match screen {
            Screen::Running(agent) if !agent.version.is_empty() => agent.version.clone(),
            _ => env!("CARGO_PKG_VERSION").to_string(),
        };
        right = right.child(format!("v{version}"));

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_5()
            .h(px(36.))
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .text_xs()
            .text_color(theme::muted())
            .child(div().min_w_0().truncate().child(message))
            .child(right)
    }
}

impl Render for PlayitWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = self.state.screen();
        let body = match &screen {
            Screen::Running(agent) => self.render_running(agent, cx),
            screen => self.render_message(screen, cx),
        };

        let mut root = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .text_color(theme::text())
            .text_sm()
            .child(self.render_header(&screen, cx));

        if let Some(error) = self.command_error.clone() {
            root = root.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_5()
                    .py_2()
                    .bg(rgb(0x3a1d24))
                    .text_color(theme::red())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(icon(Icon::Alert, 16., theme::red()))
                            .child(error),
                    )
                    .child(
                        button("dismiss-error", "Dismiss", ButtonStyle::Ghost, None).on_click(
                            cx.listener(|view, _, _, cx| {
                                view.command_error = None;
                                cx.notify();
                            }),
                        ),
                    ),
            );
        }

        root = root
            // Scrolls only when the window is too short for the fixed sections and logs.
            .child(
                div()
                    .id("content")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(body),
            )
            .child(self.render_footer(&screen, cx));

        if let Some((toast, _)) = &self.toast {
            root = root.child(
                div()
                    .absolute()
                    .bottom(px(52.))
                    .right(px(20.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(theme::accent_strong())
                    .text_color(rgb(0xffffff))
                    .shadow_lg()
                    .child(icon(Icon::Check, 14., rgb(0xffffff).into()))
                    .child(toast.clone()),
            );
        }
        root
    }
}

/// The centered card used for every state other than running.
struct MessageCard {
    icon: Icon,
    color: Hsla,
    title: SharedString,
    body: Vec<String>,
    steps: Option<[StepState; 3]>,
    link: Option<String>,
    command: Option<String>,
    details: Option<String>,
    waiting: Option<&'static str>,
    actions: Vec<AnyElement>,
}

impl Default for MessageCard {
    fn default() -> Self {
        Self {
            icon: Icon::Info,
            color: theme::muted(),
            title: SharedString::default(),
            body: Vec::new(),
            steps: None,
            link: None,
            command: None,
            details: None,
            waiting: None,
            actions: Vec::new(),
        }
    }
}

impl MessageCard {
    fn render(self, cx: &mut Context<PlayitWindow>) -> impl IntoElement {
        let mut card = panel()
            .flex()
            .flex_col()
            .gap_4()
            .p_8()
            .w(px(580.))
            .max_w_full()
            .child(
                div()
                    .size(px(48.))
                    .flex_none()
                    .min_h(px(48.))
                    .min_w(px(48.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(self.color.opacity(0.14))
                    .text_color(self.color)
                    .child(icon(self.icon, 24., self.color)),
            )
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_color(theme::muted())
                    .children(self.body.into_iter().map(|line| div().child(line))),
            );

        if let Some(steps) = self.steps {
            card = card.child(render_steps(steps));
        }

        if let Some(command) = self.command {
            let copy = command.clone();
            card =
                card.child(
                    inset()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .font_family(MONO_FONT)
                                .text_color(theme::text())
                                .child(command),
                        )
                        .child(
                            button(
                                "copy-command",
                                "Copy",
                                ButtonStyle::Secondary,
                                Some(Icon::Copy),
                            )
                            .on_click(cx.listener(
                                move |view, _, _, cx| view.copy(copy.clone(), "command", cx),
                            )),
                        ),
                );
        }

        if let Some(link) = self.link {
            card = card.child(
                inset()
                    .font_family(MONO_FONT)
                    .text_xs()
                    .text_color(theme::muted())
                    .child(link),
            );
        }

        if !self.actions.is_empty() {
            card = card.child(div().flex().flex_wrap().gap_2().children(self.actions));
        }

        if let Some(waiting) = self.waiting {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme::muted())
                    .child(icon(Icon::Clock, 14., theme::muted()))
                    .child(waiting),
            );
        }

        if let Some(details) = self.details {
            let copy = details.clone();
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .pt_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::faint())
                                    .child("TECHNICAL DETAILS"),
                            )
                            .child(
                                button(
                                    "copy-details",
                                    "Copy details",
                                    ButtonStyle::Ghost,
                                    Some(Icon::Copy),
                                )
                                .on_click(cx.listener(
                                    move |view, _, _, cx| view.copy(copy.clone(), "details", cx),
                                )),
                            ),
                    )
                    .child(
                        inset()
                            .font_family(MONO_FONT)
                            .text_xs()
                            .text_color(theme::muted())
                            .child(details),
                    ),
            );
        }

        card
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepState {
    Done,
    Active,
    Todo,
}

fn setup_progress(step: &SetupStep) -> [StepState; 3] {
    use StepState::*;
    match step {
        SetupStep::Preparing | SetupStep::WaitingForUserVisit => [Active, Todo, Todo],
        SetupStep::WaitingForUser => [Done, Active, Todo],
        SetupStep::Approved => [Done, Done, Active],
        SetupStep::Rejected | SetupStep::Failed(_) => [Todo, Todo, Todo],
    }
}

fn render_steps(steps: [StepState; 3]) -> impl IntoElement {
    const LABELS: [&str; 3] = [
        "Open the setup page",
        "Sign in and approve this computer",
        "playit starts automatically",
    ];
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(
            steps
                .into_iter()
                .zip(LABELS)
                .enumerate()
                .map(|(ix, (state, label))| {
                    let (marker_bg, marker_fg, text) = match state {
                        StepState::Done => {
                            (theme::green().opacity(0.16), theme::green(), theme::muted())
                        }
                        StepState::Active => {
                            (theme::accent_strong(), rgb(0xffffff).into(), theme::text())
                        }
                        StepState::Todo => (theme::panel_raised(), theme::faint(), theme::faint()),
                    };
                    let marker = div()
                        .size(px(22.))
                        .flex_none()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(marker_bg)
                        .text_color(marker_fg)
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD);
                    let marker = if state == StepState::Done {
                        marker.child(icon(Icon::Check, 12., marker_fg))
                    } else {
                        marker.child((ix + 1).to_string())
                    };
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .text_color(text)
                        .when(state == StepState::Active, |row| {
                            row.font_weight(FontWeight::MEDIUM)
                        })
                        .child(marker)
                        .child(label)
                }),
        )
}

struct UnreachableGuidance {
    body: Vec<String>,
    command: Option<String>,
}

fn unreachable_guidance(mode: SessionMode, error: &str) -> UnreachableGuidance {
    if mode == SessionMode::ExternalDaemon {
        return UnreachableGuidance {
            body: vec![
                "No playit agent is answering at the address this window was opened with. Start the agent and this window will connect on its own.".into(),
            ],
            command: None,
        };
    }
    if cfg!(target_os = "linux") && error.contains("Permission denied") {
        return UnreachableGuidance {
            body: vec![
                "playit is running, but your user isn't allowed to talk to it yet. Add yourself to the playit group, then log out and back in.".into(),
            ],
            command: Some("sudo usermod -aG playit $USER".into()),
        };
    }
    // `playit start` on Linux needs to know the init system, so name systemd directly.
    let command = if cfg!(target_os = "linux") {
        "sudo systemctl start playit"
    } else {
        "playit start"
    };
    UnreachableGuidance {
        body: vec![
            "The playit background service on this computer isn't running, so your tunnels are offline.".into(),
            "Start it from a terminal and this window will connect on its own:".into(),
        ],
        command: Some(command.into()),
    }
}

fn tunnel_row(
    id: impl Into<ElementId>,
    dot_color: Hsla,
    address: String,
    active: bool,
    detail: Div,
    actions: Div,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_1p5()
        .rounded_md()
        .hover(|style| style.bg(theme::panel_raised()))
        .child(dot(dot_color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div().flex().min_w_0().child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if active {
                                theme::text()
                            } else {
                                theme::muted()
                            })
                            .child(address),
                    ),
                )
                .child(div().flex().min_w_0().text_xs().child(detail)),
        )
        .child(actions)
}

fn log_row(entry: &LogEntry, today: chrono::NaiveDate) -> Div {
    let level_color = match entry.level {
        LogLevel::Error => theme::red(),
        LogLevel::Warn => theme::yellow(),
        LogLevel::Info => theme::green(),
        LogLevel::Debug => theme::blue(),
        LogLevel::Trace => theme::muted(),
    };
    div()
        .h(px(LOG_ROW_HEIGHT))
        .px_4()
        .flex()
        .items_center()
        .gap_2()
        .whitespace_nowrap()
        .child(
            div()
                .flex_none()
                .text_color(theme::faint())
                .child(log_time_label(entry.timestamp, today)),
        )
        .child(
            div()
                .flex_none()
                .w(px(44.))
                .text_color(level_color)
                .child(level_label(&entry.level)),
        )
        .child(
            div()
                .flex_none()
                .text_color(theme::faint())
                .child(format!("{}:", short_target(&entry.target))),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(theme::text())
                .child(entry.message.clone()),
        )
}

fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// SVGs don't inherit text color from their parent, so every icon names its color.
fn icon(icon: Icon, size: f32, color: Hsla) -> Svg {
    svg()
        .path(icon.path())
        .size(px(size))
        .flex_none()
        .text_color(color)
}

fn panel() -> Div {
    div()
        .bg(theme::panel())
        .border_1()
        .border_color(theme::border())
        .rounded_lg()
        .overflow_hidden()
}

fn inset() -> Div {
    div()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(theme::inset())
        .border_1()
        .border_color(theme::border())
}

fn panel_header(title: AnyElement, right: Option<AnyElement>) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(44.))
        .px_4()
        .border_b_1()
        .border_color(theme::border())
        .child(title)
        .children(right)
}

fn dot(color: Hsla) -> Div {
    div().flex_none().size(px(8.)).rounded_full().bg(color)
}

fn pill(label: &'static str, color: Hsla) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_2p5()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(color.opacity(0.4))
        .bg(color.opacity(0.12))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(dot(color))
        .child(label)
}

fn chip(label: &'static str, color: Hsla) -> impl IntoElement {
    div()
        .px_2()
        .py_0p5()
        .rounded_md()
        .bg(color.opacity(0.14))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(label)
}

fn header_stat(label: &'static str, value: AnyElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_end()
        .gap_0p5()
        .child(div().text_xs().text_color(theme::faint()).child(label))
        .child(value)
}

fn stat_cell(
    icon_kind: Icon,
    label: &'static str,
    value: String,
    divider: bool,
) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .px_4()
        .py_3()
        .when(divider, |cell| {
            cell.border_r_1().border_color(theme::border())
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_xs()
                .text_color(theme::muted())
                .child(icon(icon_kind, 14., theme::muted()))
                .child(div().truncate().child(label)),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
}

fn toggle(id: &'static str, label: &'static str, on: bool) -> Stateful<Div> {
    let marker = div()
        .size(px(14.))
        .flex_none()
        .rounded_sm()
        .flex()
        .items_center()
        .justify_center()
        .border_1();
    let marker = if on {
        marker
            .border_color(theme::accent_strong())
            .bg(theme::accent_strong())
            .text_color(rgb(0xffffff))
            .child(icon(Icon::Check, 10., rgb(0xffffff).into()))
    } else {
        marker.border_color(theme::border_strong())
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1p5()
        .cursor_pointer()
        .text_xs()
        .text_color(if on { theme::text() } else { theme::muted() })
        .hover(|style| style.text_color(theme::text()))
        .child(marker)
        .child(label)
}

#[derive(Clone, Copy)]
enum ButtonStyle {
    Primary,
    Secondary,
    Ghost,
}

fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    style: ButtonStyle,
    leading_icon: Option<Icon>,
) -> Stateful<Div> {
    let (bg, hover_bg, border, text) = match style {
        ButtonStyle::Primary => (
            theme::accent_strong(),
            theme::accent_hover(),
            theme::accent_strong(),
            rgb(0xffffff).into(),
        ),
        ButtonStyle::Secondary => (
            theme::panel_raised(),
            rgb(0x30304a).into(),
            theme::border_strong(),
            theme::text(),
        ),
        ButtonStyle::Ghost => (
            gpui::transparent_black(),
            theme::panel_raised(),
            gpui::transparent_black(),
            theme::muted(),
        ),
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .px_2p5()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(text)
        .cursor_pointer()
        .hover(move |style| style.bg(hover_bg))
        .children(leading_icon.map(|kind| icon(kind, 13., text)))
        .child(label.into())
}

/// A taller button for the main action of a screen.
fn large(button: Stateful<Div>) -> Stateful<Div> {
    button.px_4().py_2().text_sm().rounded_lg()
}

fn link_button(id: &'static str, label: &'static str, url: &str, style: ButtonStyle) -> AnyElement {
    let url = url.to_string();
    large(button(id, label, style, Some(Icon::External)))
        .on_click(move |_, _, cx| cx.open_url(&url))
        .into_any_element()
}
