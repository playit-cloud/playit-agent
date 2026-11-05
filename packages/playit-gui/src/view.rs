use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::Local;
use gpui::{
    Animation, AnimationExt, AnyElement, ClipboardItem, Context, Div, ElementId, FontWeight, Hsla,
    ScrollHandle, ScrollStrategy, SharedString, Stateful, Svg, Task, Transformation,
    UniformListScrollHandle, Window, div, linear_color_stop, linear_gradient, percentage,
    prelude::*, px, rgb, svg, uniform_list,
};
use playit_ipc::model::{AccountStatus, AgentState, LogEntry, LogLevel};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::assets::Icon;
use crate::backend::{BackendEvent, CommandOutcome, SetupStep, UiCommand};
use crate::state::{
    ACCOUNT_AGENTS_URL, ACCOUNT_UPGRADE_URL, AppState, Screen, SessionMode, disabled_reason,
    format_bytes, format_uptime, level_label, log_time_label, short_target,
};

const SUPPORT_URL: &str = "https://playit.gg/support";

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
        rgb(0xb0b0c4).into()
    }
    pub fn faint() -> Hsla {
        rgb(0x83839b).into()
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
    pub fn amber() -> Hsla {
        rgb(0xfbbf24).into()
    }
    pub fn red() -> Hsla {
        rgb(0xf87171).into()
    }
    pub fn blue() -> Hsla {
        rgb(0x60a5fa).into()
    }
    pub fn white() -> Hsla {
        rgb(0xffffff).into()
    }
}

#[cfg(target_os = "macos")]
const MONO_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
const MONO_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO_FONT: &str = "DejaVu Sans Mono";

const LOG_ROW_HEIGHT: f32 = 20.;
const CARD_WIDTH: f32 = 580.;
const TOAST_DURATION: Duration = Duration::from_secs(2);
/// Windows shorter than this start with the activity log collapsed and use tighter spacing.
const COMPACT_HEIGHT: f32 = 640.;
/// Windows narrower than this drop secondary text that would crowd the tunnel header.
const NARROW_WIDTH: f32 = 800.;

pub struct PlayitWindow {
    state: AppState,
    commands: UnboundedSender<UiCommand>,
    content_scroll: ScrollHandle,
    log_scroll: UniformListScrollHandle,
    follow_logs: bool,
    show_debug_logs: bool,
    /// `None` follows the default: open while running in a roomy window, closed otherwise.
    logs_expanded: Option<bool>,
    dismissed_notices: HashSet<String>,
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
            content_scroll: ScrollHandle::new(),
            log_scroll: UniformListScrollHandle::new(),
            follow_logs: true,
            show_debug_logs: false,
            logs_expanded: None,
            dismissed_notices: HashSet::new(),
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

    fn open_dashboard(&mut self, cx: &mut Context<Self>) {
        self.send(UiCommand::OpenDashboard);
        cx.notify();
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
            Screen::Connecting => ("Connecting", theme::amber()),
            Screen::Starting => ("Starting", theme::amber()),
            Screen::Stopping => ("Stopping", theme::amber()),
            Screen::Setup(_) => ("Not set up", theme::blue()),
            Screen::Unreachable { .. } => ("Offline", theme::red()),
            Screen::InvalidSecret { .. } => ("Sign-in problem", theme::amber()),
            Screen::OverLimit => ("Agent limit reached", theme::amber()),
            Screen::Error { .. } => ("Error", theme::red()),
        };

        let mut right = div().flex().items_center().gap_5();
        if let Screen::Running(agent) = screen {
            right = right
                .child(header_stat(
                    "Uptime",
                    div()
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
                    .on_click(cx.listener(|view, _, _, cx| view.open_dashboard(cx))),
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
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .size(px(28.))
                                    .flex_none()
                                    .rounded_lg()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(theme::accent_strong())
                                    .child(icon(Icon::Play, 16., theme::white())),
                            )
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .child("playit"),
                            ),
                    )
                    .child(pill(status_label, status_color)),
            )
            .child(right)
    }

    fn render_account(&self, status: &AccountStatus, cx: &mut Context<Self>) -> AnyElement {
        let row = div().flex().items_center().gap_1p5();
        match status {
            AccountStatus::Verified => row
                .child(icon(Icon::CheckCircle, 14., theme::green()))
                .child("Verified")
                .into_any_element(),
            AccountStatus::EmailNotVerified => row
                .text_color(theme::amber())
                .child(icon(Icon::Alert, 14., theme::amber()))
                .child("Email not verified")
                .into_any_element(),
            AccountStatus::Guest => row
                .gap_2()
                .child(icon(Icon::User, 14., theme::muted()))
                .child("Guest")
                .child(
                    div()
                        .id("save-account")
                        .px_1p5()
                        .rounded_sm()
                        .border_1()
                        .border_color(theme::accent().opacity(0.5))
                        .text_xs()
                        .text_color(theme::accent())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::accent().opacity(0.12)))
                        .child("Save account")
                        .on_click(cx.listener(|view, _, _, cx| view.open_dashboard(cx))),
                )
                .into_any_element(),
            AccountStatus::Unknown => row
                .text_color(theme::muted())
                .child("Unknown")
                .into_any_element(),
        }
    }

    fn render_running(
        &self,
        agent: &AgentState,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .when(compact, |body| body.gap_3())
            .when(!compact, |body| body.gap_4());

        for (ix, notice) in agent.notices.iter().enumerate() {
            if self.dismissed_notices.contains(&notice.message) {
                continue;
            }
            body = body.child(self.render_notice(ix, notice, cx));
        }

        body.child(self.render_tunnels(agent, narrow, cx))
            .child(self.render_stats())
            .child(self.render_logs(!compact, cx))
    }

    fn render_notice(
        &self,
        ix: usize,
        notice: &playit_ipc::model::NoticeState,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let color = match notice.priority.to_ascii_lowercase().as_str() {
            "high" | "critical" | "error" => theme::red(),
            "low" | "info" => theme::blue(),
            _ => theme::amber(),
        };
        let message = notice.message.clone();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_1p5()
            .rounded_lg()
            .border_1()
            .border_color(color.opacity(0.35))
            .bg(color.opacity(0.08))
            .child(icon(Icon::Info, 15., color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(notice.message.clone()),
            )
            .children(notice.resolve_link.clone().map(|link| {
                div()
                    .id(("notice-link", ix))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .text_color(color)
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child("Learn more")
                    .child(icon(Icon::External, 13., color))
                    .on_click(move |_, _, cx| cx.open_url(&link))
            }))
            .child(
                div()
                    .id(("dismiss-notice", ix))
                    .flex_none()
                    .p_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::panel_raised()))
                    .child(icon(Icon::X, 14., theme::muted()))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.dismissed_notices.insert(message.clone());
                        cx.notify();
                    })),
            )
    }

    fn render_tunnels(
        &self,
        agent: &AgentState,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let has_tunnels = !agent.tunnels.is_empty() || !agent.pending_tunnels.is_empty();
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
            .max_h(px(320.))
            .overflow_y_scroll();

        if !has_tunnels {
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
                            .size(px(44.))
                            .min_h(px(44.))
                            .flex_none()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme::accent().opacity(0.12))
                            .child(icon(Icon::Plus, 22., theme::accent())),
                    )
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("No tunnels yet"),
                    )
                    .child(
                        div()
                            .w(px(420.))
                            .max_w_full()
                            .text_color(theme::muted())
                            .text_center()
                            .child("A tunnel gives your game server a public address your friends can join. Create one on playit.gg and it will show up here automatically."),
                    )
                    .child(
                        large(button(
                            "add-tunnel",
                            "Create a tunnel",
                            ButtonStyle::Primary,
                            Some(Icon::External),
                        ))
                        .on_click(cx.listener(|view, _, _, cx| view.open_dashboard(cx))),
                    ),
            );
        }

        for (ix, tunnel) in agent.tunnels.iter().enumerate() {
            if tunnel.is_disabled {
                list = list.child(tunnel_row(
                    ("tunnel", ix),
                    theme::amber(),
                    tunnel.display_address.clone(),
                    false,
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .text_color(theme::amber())
                        .child(icon(Icon::Alert, 12., theme::amber()))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(disabled_reason(tunnel.disabled_reason.as_deref())),
                        ),
                    Some(chip("Disabled", theme::amber()).into_any_element()),
                    button(
                        ("manage-tunnel", ix),
                        "Manage",
                        ButtonStyle::Secondary,
                        Some(Icon::External),
                    )
                    .on_click(cx.listener(|view, _, _, cx| view.open_dashboard(cx)))
                    .into_any_element(),
                ));
            } else {
                let address = tunnel.display_address.clone();
                list =
                    list.child(tunnel_row(
                        ("tunnel", ix),
                        theme::green(),
                        tunnel.display_address.clone(),
                        true,
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .min_w_0()
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme::faint())
                                    .child("Forwards to"),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(MONO_FONT)
                                    .text_color(theme::muted())
                                    .child(tunnel.destination.clone()),
                            ),
                        None,
                        button(
                            ("copy-tunnel", ix),
                            "Copy address",
                            ButtonStyle::Secondary,
                            Some(Icon::Copy),
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.copy(address.clone(), "address", cx)
                        }))
                        .into_any_element(),
                    ));
            }
        }

        for (ix, pending) in agent.pending_tunnels.iter().enumerate() {
            list = list.child(tunnel_row(
                ("pending", ix),
                theme::blue(),
                pending.id.clone(),
                false,
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .min_w_0()
                    .text_color(theme::muted())
                    .child(spinner(("pending-spinner", ix), 12., theme::blue()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(format!("{}…", sentence_case(&pending.status_msg))),
                    ),
                Some(chip("Setting up", theme::blue()).into_any_element()),
                div().into_any_element(),
            ));
        }

        let title = div()
            .flex()
            .flex_col()
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Tunnels"),
            )
            .when(has_tunnels, |title| {
                title.child(
                    div()
                        .text_xs()
                        .text_color(theme::muted())
                        .child("Share a public address so friends can join your server."),
                )
            });

        let header_right = has_tunnels.then(|| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .when(!narrow, |right| {
                    right.child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(summary.join(" · ")),
                    )
                })
                .child(
                    button(
                        "new-tunnel",
                        "Add tunnel",
                        ButtonStyle::Secondary,
                        Some(Icon::Plus),
                    )
                    .on_click(cx.listener(|view, _, _, cx| view.open_dashboard(cx))),
                )
                .into_any_element()
        });

        panel()
            .flex()
            .flex_col()
            .flex_none()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_4()
                    .py_2p5()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(title)
                    .children(header_right),
            )
            .child(list)
    }

    fn render_stats(&self) -> impl IntoElement {
        let stats = &self.state.stats;
        panel()
            .flex_none()
            .flex()
            .child(stat_cell(
                Icon::ArrowDown,
                "Data received",
                format_bytes(stats.bytes_in),
                "since start".to_string(),
                true,
            ))
            .child(stat_cell(
                Icon::ArrowUp,
                "Data sent",
                format_bytes(stats.bytes_out),
                "since start".to_string(),
                true,
            ))
            .child(stat_cell(
                Icon::Plug,
                "Active connections",
                (stats.active_tcp + stats.active_udp).to_string(),
                format!("{} TCP · {} UDP", stats.active_tcp, stats.active_udp),
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
                    .text_base()
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
                    toggle("debug-logs", "Debug details", self.show_debug_logs).on_click(
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

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_3()
            .h(px(44.))
            .px_4()
            .when(expanded, |header| {
                header.border_b_1().border_color(theme::border())
            })
            .child(title)
            .children(controls);

        let panel = panel().flex().flex_col().child(header);
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
                let show_target = view.show_debug_logs;
                view.state
                    .logs
                    .iter()
                    .filter(|entry| view.shows_log(entry))
                    .skip(range.start)
                    .take(range.len())
                    .map(|entry| log_row(entry, today, show_target))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.log_scroll.clone())
        .flex_1();

        panel.flex_1().min_h(px(130.)).child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .py_1()
                .font_family(MONO_FONT)
                .text_xs()
                .child(list),
        )
    }

    fn render_message(&self, screen: &Screen, cx: &mut Context<Self>) -> Div {
        let mut card = MessageCard::default();

        match screen {
            Screen::Connecting => {
                card.badge = Badge::Spinner;
                card.color = theme::amber();
                card.title = "Connecting to playit".into();
                card.body =
                    vec!["Looking for the playit background service on this computer…".into()];
            }
            Screen::Unreachable { error } => {
                card.badge = Badge::Icon(Icon::Power);
                card.color = theme::red();
                card.title = "Can't connect to playit".into();
                let guidance = unreachable_guidance(self.state.session_mode, error);
                card.body = guidance.body;
                card.command = guidance.command;
                card.details = Some(format!("Address: {}\n{}", self.state.socket_path, error));
                card.waiting = Some("Trying again every few seconds…");
            }
            Screen::Starting => {
                card.badge = Badge::Spinner;
                card.color = theme::amber();
                card.title = "Starting playit".into();
                card.body = vec![
                    "Connecting to the playit network. This usually takes a few seconds.".into(),
                ];
            }
            Screen::Stopping => {
                card.badge = Badge::Spinner;
                card.color = theme::amber();
                card.title = "playit is stopping".into();
                card.body = vec!["Your tunnels are going offline.".into()];
            }
            Screen::Error { message } => {
                card.badge = Badge::Icon(Icon::Alert);
                card.color = theme::red();
                card.title = "playit ran into a problem".into();
                card.body = vec![
                    "playit is retrying automatically. If this keeps happening, copy the error details and send them to playit support.".into(),
                ];
                card.details = Some(message.to_string());
                let details = message.to_string();
                card.actions.push(
                    large(button(
                        "copy-error",
                        "Copy error details",
                        ButtonStyle::Primary,
                        Some(Icon::Copy),
                    ))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.copy(details.clone(), "error details", cx)
                    }))
                    .into_any_element(),
                );
                card.actions.push(link_button(
                    "get-help",
                    "Get help",
                    SUPPORT_URL,
                    ButtonStyle::Secondary,
                ));
                card.details_copyable = false;
            }
            Screen::InvalidSecret { message } => {
                card.badge = Badge::Icon(Icon::Link);
                card.color = theme::amber();
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
                card.badge = Badge::Icon(Icon::Alert);
                card.color = theme::amber();
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
                card.badge = Badge::Icon(Icon::Link);
                card.color = theme::accent();
                card.title = "Connect playit to your account".into();
                card.body = vec![
                    "Link this computer to your playit account to start sharing your servers."
                        .into(),
                ];
                match &setup.step {
                    SetupStep::Rejected | SetupStep::Failed(_) => {
                        card.badge = Badge::Icon(Icon::Alert);
                        card.color = theme::red();
                        card.body = vec![match &setup.step {
                            SetupStep::Failed(error) => format!("Setup didn't finish: {error}"),
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
                            card.actions.push(link_button(
                                "open-setup",
                                "Open setup page",
                                &url,
                                ButtonStyle::Primary,
                            ));
                            card.link = Some(url);
                        }
                    }
                }
            }
            Screen::Running(_) => unreachable!("running screen has its own layout"),
        }

        // Spacers weighted 1:2 hold the card a third of the way down, so it doesn't jump
        // as much between states as a centered card would.
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .child(div().flex_1().min_h(px(16.)))
            .child(div().flex().justify_center().child(card.render(cx)))
            .child(div().flex_1().min_h(px(16.)))
            .child(div().flex_1());
        if !self.state.logs.is_empty() {
            body = body.child(self.render_logs(false, cx));
        }
        body
    }

    fn render_footer(&self, screen: &Screen, cx: &mut Context<Self>) -> impl IntoElement {
        let message = match screen {
            Screen::Running(_) => {
                Some("playit keeps running in the background after you close this window.")
            }
            _ => None,
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
            .child(div().min_w_0().truncate().children(message))
            .child(right)
    }
}

impl Render for PlayitWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let compact = viewport.height < px(COMPACT_HEIGHT);
        let narrow = viewport.width < px(NARROW_WIDTH);
        let screen = self.state.screen();
        let body = match &screen {
            Screen::Running(agent) => self.render_running(agent, compact, narrow, cx),
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

        // The content scrolls only when the window is too short for it. A fade and a hint
        // at the bottom edge show that more is below.
        let max_scroll = self.content_scroll.max_offset().height;
        let more_below =
            max_scroll > px(1.) && -self.content_scroll.offset().y < max_scroll - px(1.);
        root = root
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("content")
                            .flex()
                            .flex_col()
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.content_scroll)
                            .when(compact, |content| content.p_3())
                            .when(!compact, |content| content.p_4())
                            .child(body),
                    )
                    .when(more_below, |area| {
                        area.child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .h(px(56.))
                                .flex()
                                .items_end()
                                .justify_center()
                                .pb_2()
                                .bg(linear_gradient(
                                    180.,
                                    linear_color_stop(theme::bg().opacity(0.), 0.),
                                    linear_color_stop(theme::bg(), 0.85),
                                ))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .px_2p5()
                                        .py_0p5()
                                        .rounded_full()
                                        .bg(theme::panel_raised())
                                        .border_1()
                                        .border_color(theme::border_strong())
                                        .text_xs()
                                        .text_color(theme::muted())
                                        .child("Scroll for more")
                                        .child(icon(Icon::ArrowDown, 12., theme::muted())),
                                ),
                        )
                    }),
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
                    .text_color(theme::white())
                    .shadow_lg()
                    .child(icon(Icon::Check, 14., theme::white()))
                    .child(toast.clone()),
            );
        }
        root
    }
}

enum Badge {
    Icon(Icon),
    Spinner,
}

/// The card used for every state other than running.
struct MessageCard {
    badge: Badge,
    color: Hsla,
    title: SharedString,
    body: Vec<String>,
    steps: Option<[StepState; 3]>,
    link: Option<String>,
    command: Option<String>,
    details: Option<String>,
    /// The error screen already offers a primary copy button for its details.
    details_copyable: bool,
    waiting: Option<&'static str>,
    actions: Vec<AnyElement>,
}

impl Default for MessageCard {
    fn default() -> Self {
        Self {
            badge: Badge::Icon(Icon::Info),
            color: theme::muted(),
            title: SharedString::default(),
            body: Vec::new(),
            steps: None,
            link: None,
            command: None,
            details: None,
            details_copyable: true,
            waiting: None,
            actions: Vec::new(),
        }
    }
}

impl MessageCard {
    fn render(self, cx: &mut Context<PlayitWindow>) -> impl IntoElement {
        let badge: AnyElement = match self.badge {
            Badge::Icon(kind) => icon(kind, 24., self.color).into_any_element(),
            Badge::Spinner => spinner("badge-spinner", 24., self.color).into_any_element(),
        };
        let mut card = panel()
            .flex()
            .flex_col()
            .gap_4()
            .p_8()
            .w(px(CARD_WIDTH))
            .max_w_full()
            .child(
                div()
                    .size(px(48.))
                    .min_h(px(48.))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(self.color.opacity(0.14))
                    .child(badge),
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
                    // A definite width keeps text measurement and painting in agreement;
                    // otherwise lines that wrap near the edge reserve an extra blank line.
                    .children(
                        self.body
                            .into_iter()
                            .map(|line| div().w(px(CARD_WIDTH - 64.)).max_w_full().child(line)),
                    ),
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

        if !self.actions.is_empty() {
            card = card.child(div().flex().gap_2().children(self.actions));
        }

        if let Some(link) = self.link {
            let copy = link.clone();
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .child("Or open this link on any device:"),
                    )
                    .child(
                        inset()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(MONO_FONT)
                                    .text_xs()
                                    .text_color(theme::text())
                                    .child(link),
                            )
                            .child(
                                button(
                                    "copy-link",
                                    "Copy link",
                                    ButtonStyle::Secondary,
                                    Some(Icon::Copy),
                                )
                                .on_click(cx.listener(
                                    move |view, _, _, cx| view.copy(copy.clone(), "link", cx),
                                )),
                            ),
                    ),
            );
        }

        if let Some(waiting) = self.waiting {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme::muted())
                    .child(spinner("waiting-spinner", 14., theme::muted()))
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
                    .pt_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(px(26.))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::muted())
                                    .child("Technical details"),
                            )
                            .when(self.details_copyable, |header| {
                                header.child(
                                    button(
                                        "copy-details",
                                        "Copy details",
                                        ButtonStyle::Secondary,
                                        Some(Icon::Copy),
                                    )
                                    .on_click(cx.listener(
                                        move |view, _, _, cx| {
                                            view.copy(copy.clone(), "details", cx)
                                        },
                                    )),
                                )
                            }),
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
        "Once approved, playit connects automatically",
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
                            (theme::accent_strong(), theme::white(), theme::text())
                        }
                        StepState::Todo => (theme::panel_raised(), theme::muted(), theme::muted()),
                    };
                    let marker = div()
                        .size(px(22.))
                        .min_h(px(22.))
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
                "The playit agent this window watches isn't responding. Start it, and this window will reconnect automatically.".into(),
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
            "The playit background service on this computer isn't running or isn't responding, so your tunnels are offline.".into(),
            "Start it from a terminal, and this window will reconnect automatically:".into(),
        ],
        command: Some(command.into()),
    }
}

/// One tunnel: status dot, address over a detail line, an optional status chip, and an
/// action in a fixed-width slot so every row's right edge lines up.
fn tunnel_row(
    id: impl Into<ElementId>,
    dot_color: Hsla,
    address: String,
    active: bool,
    detail: Div,
    status: Option<AnyElement>,
    action: AnyElement,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
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
        .children(status)
        .child(
            div()
                .flex()
                .flex_none()
                .justify_end()
                .w(px(124.))
                .child(action),
        )
}

fn log_row(entry: &LogEntry, today: chrono::NaiveDate, show_target: bool) -> Div {
    let (level_color, message_color, row_bg) = match entry.level {
        LogLevel::Error => (theme::red(), theme::red(), Some(theme::red().opacity(0.07))),
        LogLevel::Warn => (theme::amber(), theme::amber(), None),
        LogLevel::Info => (theme::green(), theme::muted(), None),
        LogLevel::Debug => (theme::blue(), theme::faint(), None),
        LogLevel::Trace => (theme::faint(), theme::faint(), None),
    };
    div()
        .w_full()
        .h(px(LOG_ROW_HEIGHT))
        .px_4()
        .flex()
        .items_center()
        .gap_2()
        .whitespace_nowrap()
        .when_some(row_bg, |row, bg| row.bg(bg))
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
        .when(show_target, |row| {
            row.child(
                div()
                    .flex_none()
                    .text_color(theme::faint())
                    .child(format!("{}:", short_target(&entry.target))),
            )
        })
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(message_color)
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

fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    icon(Icon::Loader, size, color).with_animation(
        id,
        Animation::new(Duration::from_millis(900)).repeat(),
        |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
    )
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

fn chip(label: &'static str, color: Hsla) -> Div {
    div()
        .flex_none()
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
        .child(div().text_xs().text_color(theme::muted()).child(label))
        .child(value)
}

fn stat_cell(
    icon_kind: Icon,
    label: &'static str,
    value: String,
    caption: String,
    divider: bool,
) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .px_4()
        .py_2p5()
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
                .flex()
                .items_baseline()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex_none()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme::faint())
                        .child(caption),
                ),
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
            .child(icon(Icon::Check, 10., theme::white()))
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
            theme::white(),
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
