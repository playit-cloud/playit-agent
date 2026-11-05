use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::Local;
use gpui::{
    Animation, AnimationExt, AnyElement, ClipboardItem, Context, Div, ElementId, FontWeight, Hsla,
    ScrollHandle, ScrollStrategy, SharedString, Stateful, Svg, Task, Transformation,
    UniformListScrollHandle, Window, div, linear_color_stop, linear_gradient, percentage,
    prelude::*, px, rgb, svg, uniform_list,
};
use playit_ipc::model::{AccountStatus, AgentState, LogEntry, LogLevel, NoticeState};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::assets::Icon;
use crate::backend::{BackendEvent, CommandOutcome, SetupStep, UiCommand};
use crate::state::{
    ACCOUNT_AGENTS_URL, ACCOUNT_UPGRADE_URL, AppState, Screen, SessionMode, disabled_reason,
    format_bytes, format_uptime, level_label, log_time_label, short_target,
};

const SUPPORT_URL: &str = "https://playit.gg/support";

/// Colors carry one meaning each: green is online, blue is in progress, amber needs the
/// user's attention, and red means something failed.
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
        rgb(0xbcbcd0).into()
    }
    pub fn faint() -> Hsla {
        rgb(0xa0a0b8).into()
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
    pub fn online() -> Hsla {
        rgb(0x4ade80).into()
    }
    pub fn progress() -> Hsla {
        rgb(0x60a5fa).into()
    }
    pub fn attention() -> Hsla {
        rgb(0xfbbf24).into()
    }
    pub fn failure() -> Hsla {
        rgb(0xf87171).into()
    }
    pub fn white() -> Hsla {
        rgb(0xffffff).into()
    }
}

// GPUI maps the Linux system UI font to IBM Plex Sans, which most desktops lack, and its
// fallback face has no bold weight. Noto Sans ships with most distributions.
#[cfg(target_os = "macos")]
const UI_FONT: &str = ".SystemUIFont";
#[cfg(target_os = "windows")]
const UI_FONT: &str = "Segoe UI";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const UI_FONT: &str = "Noto Sans";

#[cfg(target_os = "macos")]
const MONO_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
const MONO_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO_FONT: &str = "DejaVu Sans Mono";

const LOG_ROW_HEIGHT: f32 = 22.;
/// An expanded log shows at most this many lines before it scrolls.
const LOG_MAX_ROWS: usize = 8;
const CARD_WIDTH: f32 = 580.;
/// `p_8` on both sides plus the 1px border.
const CARD_PADDING_AND_BORDER: f32 = 66.;
/// Width of the action column on tunnel rows, so every row's button lines up.
const ROW_ACTION_WIDTH: f32 = 112.;
const TOAST_DURATION: Duration = Duration::from_secs(2);
/// Starting longer than this gets a "taking longer than usual" hint.
const SLOW_START: Duration = Duration::from_secs(20);
/// Windows shorter than this use tighter spacing.
const COMPACT_HEIGHT: f32 = 640.;

const GUEST_NOTICE: &str = "You're using a guest account. Save it to keep your tunnels if you reinstall or switch computers.";

pub struct PlayitWindow {
    state: AppState,
    commands: UnboundedSender<UiCommand>,
    content_scroll: ScrollHandle,
    log_scroll: UniformListScrollHandle,
    follow_logs: bool,
    show_debug_logs: bool,
    logs_expanded: bool,
    details_expanded: bool,
    setup_link_opened: bool,
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
            logs_expanded: false,
            details_expanded: false,
            setup_link_opened: false,
            dismissed_notices: HashSet::new(),
            toast: None,
            command_error: None,
            _tasks: vec![events_task, outcomes_task, ticker],
        }
    }

    fn after_state_change(&mut self) {
        if self.state.needs_setup() {
            self.state.begin_setup();
            self.setup_link_opened = false;
            self.send(UiCommand::StartSetup);
        }
        let visible = self.visible_logs().len();
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

    fn visible_logs(&self) -> Vec<&LogEntry> {
        self.state
            .logs
            .iter()
            .filter(|entry| {
                self.show_debug_logs || !matches!(entry.level, LogLevel::Debug | LogLevel::Trace)
            })
            .collect()
    }

    fn render_header(
        &self,
        screen: &Screen,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (status_label, status_color) = match screen {
            Screen::Running(_) => ("Connected to playit", theme::online()),
            Screen::Connecting => ("Connecting", theme::progress()),
            Screen::Starting => ("Starting", theme::progress()),
            Screen::Stopping => ("Stopping", theme::progress()),
            Screen::Setup(_) => ("Not set up", theme::accent()),
            Screen::Unreachable { .. } => ("Service not responding", theme::failure()),
            Screen::InvalidSecret { .. } => ("Sign-in problem", theme::attention()),
            Screen::OverLimit { .. } => ("Account limit reached", theme::attention()),
            Screen::Error { message } => (error_hint(message).pill, theme::failure()),
        };

        let mut right = div().flex().items_center().gap_5();
        if let Screen::Running(agent) = screen {
            right = right
                .child(header_stat(
                    "Online for",
                    div()
                        .child(format_uptime(agent.start_time, now_millis()))
                        .into_any_element(),
                ))
                .child(header_stat(
                    "Account",
                    render_account(&agent.account_status),
                ))
                .child(
                    button(
                        "dashboard",
                        "Open dashboard",
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
            .h(px(if compact { 56. } else { 64. }))
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

    fn render_running(&self, agent: &AgentState, compact: bool, cx: &mut Context<Self>) -> Div {
        let has_tunnels = !agent.tunnels.is_empty() || !agent.pending_tunnels.is_empty();
        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .when(compact, |body| body.gap_3())
            .when(!compact, |body| body.gap_4());

        if matches!(agent.account_status, AccountStatus::Guest)
            && !self.dismissed_notices.contains(GUEST_NOTICE)
        {
            body = body.child(self.render_notice(
                "guest",
                GUEST_NOTICE,
                theme::progress(),
                NoticeAction::SaveAccount,
                compact,
                cx,
            ));
        }
        for (ix, notice) in agent.notices.iter().enumerate() {
            if self.dismissed_notices.contains(&notice.message) {
                continue;
            }
            let action = match &notice.resolve_link {
                Some(link) => NoticeAction::Link(link.clone()),
                None => NoticeAction::None,
            };
            body = body.child(self.render_notice(
                ("notice", ix),
                &notice.message,
                notice_color(notice),
                action,
                compact,
                cx,
            ));
        }

        body = body.child(self.render_tunnels(agent, compact, cx));
        if has_tunnels {
            body = body.child(self.render_stats());
        }
        body.child(self.render_logs(true, cx))
    }

    fn render_notice(
        &self,
        id: impl Into<ElementId>,
        message: &str,
        color: Hsla,
        action: NoticeAction,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let dismissed = message.to_string();
        let action: Option<AnyElement> = match action {
            NoticeAction::None => None,
            NoticeAction::Link(link) => Some(
                notice_link("notice-link", "Learn more", color)
                    .on_click(move |_, _, cx| cx.open_url(&link))
                    .into_any_element(),
            ),
            NoticeAction::SaveAccount => Some(
                notice_link("save-account", "Save guest account", color)
                    .on_click(cx.listener(|view, _, _, cx| view.open_dashboard(cx)))
                    .into_any_element(),
            ),
        };
        div()
            .id(id.into())
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .when(compact, |notice| notice.py_1())
            .when(!compact, |notice| notice.py_1p5())
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
                    .child(message.to_string()),
            )
            .children(action)
            .child(
                div()
                    .id("dismiss-notice")
                    .flex_none()
                    .p_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::panel_raised()))
                    .child(icon(Icon::X, 14., theme::muted()))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.dismissed_notices.insert(dismissed.clone());
                        cx.notify();
                    })),
            )
    }

    fn render_tunnels(
        &self,
        agent: &AgentState,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let has_tunnels = !agent.tunnels.is_empty() || !agent.pending_tunnels.is_empty();
        if !has_tunnels {
            return self.render_no_tunnels(cx);
        }

        let online = agent.tunnels.iter().filter(|t| !t.is_disabled).count();
        let disabled = agent.tunnels.len() - online;
        let mut summary = vec![format!("{online} connected")];
        if disabled > 0 {
            summary.push(format!("{disabled} disabled"));
        }
        if !agent.pending_tunnels.is_empty() {
            summary.push(format!("{} setting up", agent.pending_tunnels.len()));
        }

        let forwards_to = |destination: &str| {
            div()
                .flex()
                .items_center()
                .gap_1()
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
                        .text_color(theme::muted())
                        .child(destination_label(destination)),
                )
        };

        let mut rows: Vec<AnyElement> = Vec::new();
        for (ix, tunnel) in agent.tunnels.iter().enumerate() {
            if tunnel.is_disabled {
                rows.push(
                    tunnel_row(
                        ("tunnel", ix),
                        compact,
                        TunnelRow {
                            dot_color: theme::attention(),
                            address: tunnel.display_address.clone(),
                            dimmed: true,
                            has_address: true,
                            status: Some((
                                format!(
                                    "Disabled · {}",
                                    disabled_reason(tunnel.disabled_reason.as_deref())
                                        .to_lowercase()
                                )
                                .into(),
                                theme::attention(),
                            )),
                            detail: forwards_to(&tunnel.destination),
                            action: row_button(("manage-tunnel", ix), "Fix tunnel", Icon::External)
                                .on_click(cx.listener(|view, _, _, cx| view.open_dashboard(cx)))
                                .into_any_element(),
                        },
                    )
                    .into_any_element(),
                );
            } else {
                let address = tunnel.display_address.clone();
                rows.push(
                    tunnel_row(
                        ("tunnel", ix),
                        compact,
                        TunnelRow {
                            dot_color: theme::online(),
                            address: tunnel.display_address.clone(),
                            dimmed: false,
                            has_address: true,
                            status: None,
                            detail: forwards_to(&tunnel.destination),
                            action: row_button(("copy-tunnel", ix), "Copy", Icon::Copy)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.copy(address.clone(), "address", cx)
                                }))
                                .into_any_element(),
                        },
                    )
                    .into_any_element(),
                );
            }
        }

        for (ix, pending) in agent.pending_tunnels.iter().enumerate() {
            rows.push(
                tunnel_row(
                    ("pending", ix),
                    compact,
                    TunnelRow {
                        dot_color: theme::progress(),
                        address: pending.id.clone(),
                        dimmed: false,
                        has_address: false,
                        status: Some(("Setting up".into(), theme::progress())),
                        detail: div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .min_w_0()
                            .text_color(theme::muted())
                            .child(spinner(("pending-spinner", ix), 12., theme::progress()))
                            .child(div().min_w_0().truncate().child(format!(
                                "{}. The address appears here when it's ready.",
                                sentence_case(&pending.status_msg)
                            ))),
                        action: div().into_any_element(),
                    },
                )
                .into_any_element(),
            );
        }

        let title = div()
            .flex()
            .flex_col()
            .gap_0p5()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_3()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Tunnels"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(summary.join(" · ")),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::muted())
                    .child("Share an address with friends. Your game server must also be running for them to join."),
            );

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
                    .child(
                        button(
                            "new-tunnel",
                            "Add tunnel",
                            ButtonStyle::Secondary,
                            Some(Icon::External),
                        )
                        .on_click(cx.listener(|view, _, _, cx| view.open_dashboard(cx))),
                    ),
            )
            .child(div().flex().flex_col().children(rows))
            .into_any_element()
    }

    fn render_no_tunnels(&self, cx: &mut Context<Self>) -> AnyElement {
        panel()
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .gap_3()
            .px_8()
            .py_10()
            .child(
                div()
                    .size(px(48.))
                    .min_h(px(48.))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::accent().opacity(0.12))
                    .child(icon(Icon::Globe, 24., theme::accent())),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Add your first tunnel"),
            )
            .child(
                div()
                    .w(px(440.))
                    .max_w_full()
                    .text_color(theme::muted())
                    .text_center()
                    .child("A tunnel gives your game server a public address your friends can join. Create one on playit.gg and it shows up here automatically."),
            )
            .child(
                large(button(
                    "add-tunnel",
                    "Add a tunnel on playit.gg",
                    ButtonStyle::Primary,
                    Some(Icon::External),
                ))
                .on_click(cx.listener(|view, _, _, cx| view.open_dashboard(cx))),
            )
            .into_any_element()
    }

    fn render_stats(&self) -> impl IntoElement {
        let stats = &self.state.stats;
        panel()
            .flex_none()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_3()
                    .px_4()
                    .pt_2p5()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Traffic"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .child("since playit started"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .child(stat_cell(
                        Icon::ArrowDown,
                        "Received",
                        format_bytes(stats.bytes_in),
                        None,
                        true,
                    ))
                    .child(stat_cell(
                        Icon::ArrowUp,
                        "Sent",
                        format_bytes(stats.bytes_out),
                        None,
                        true,
                    ))
                    .child(stat_cell(
                        Icon::Plug,
                        "Active connections",
                        (stats.active_tcp + stats.active_udp).to_string(),
                        Some(format!(
                            "{} TCP · {} UDP",
                            stats.active_tcp, stats.active_udp
                        )),
                        false,
                    )),
            )
    }

    /// The activity log starts collapsed. Its header counts recorded problems, quietly while
    /// the agent is healthy, so they stay findable without reading as current faults.
    fn render_logs(&self, healthy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let expanded = self.logs_expanded;
        let visible = self.visible_logs();
        let hidden = self.state.logs.len() - visible.len();
        let warnings = visible
            .iter()
            .filter(|entry| matches!(entry.level, LogLevel::Warn))
            .count();
        let errors = visible
            .iter()
            .filter(|entry| matches!(entry.level, LogLevel::Error))
            .count();

        let mut summary = div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme::muted())
            .child(format!(
                "{} {}",
                visible.len(),
                if visible.len() == 1 {
                    "message"
                } else {
                    "messages"
                }
            ));
        // On recovery screens the card already explains the problem, so the counts would
        // only repeat it.
        if healthy && warnings > 0 {
            summary = summary.child(count_badge(
                format!(
                    "{warnings} {} logged",
                    if warnings == 1 { "warning" } else { "warnings" }
                ),
                theme::attention(),
            ));
        }
        if healthy && errors > 0 {
            summary = summary.child(count_badge(
                format!(
                    "{errors} {} logged",
                    if errors == 1 { "error" } else { "errors" }
                ),
                theme::failure(),
            ));
        }

        let title = div()
            .id("toggle-logs")
            .flex()
            .flex_1()
            .items_center()
            .gap_3()
            .h_full()
            .cursor_pointer()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
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
                    ),
            )
            .child(summary)
            .on_click(cx.listener(move |view, _, _, cx| {
                view.logs_expanded = !expanded;
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
                    toggle(
                        "debug-logs",
                        if hidden > 0 && !self.show_debug_logs {
                            SharedString::from(format!("Show {hidden} technical"))
                        } else {
                            SharedString::from("Show technical")
                        },
                        self.show_debug_logs,
                    )
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.show_debug_logs = !view.show_debug_logs;
                        view.after_state_change();
                        cx.notify();
                    })),
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

        let panel = panel().flex().flex_col().flex_none().child(header);
        if !expanded {
            return panel;
        }
        if visible.is_empty() {
            return panel.child(
                div()
                    .px_4()
                    .py_3()
                    .text_color(theme::faint())
                    .child("Nothing to show yet."),
            );
        }

        let rows = visible.len().min(LOG_MAX_ROWS);
        let list = uniform_list(
            "logs",
            visible.len(),
            cx.processor(|view, range: std::ops::Range<usize>, _window, _cx| {
                let today = Local::now().date_naive();
                let show_target = view.show_debug_logs;
                view.visible_logs()[range]
                    .iter()
                    .map(|entry| log_row(entry, today, show_target))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.log_scroll.clone())
        .h(px(rows as f32 * LOG_ROW_HEIGHT));

        panel.child(
            div()
                .relative()
                .py_1()
                .font_family(MONO_FONT)
                .text_size(px(12.5))
                .child(list)
                .when(visible.len() > LOG_MAX_ROWS, |area| {
                    // A fade over the top edge hides the partly scrolled-off line.
                    area.child(div().absolute().top_0().left_0().right_0().h(px(16.)).bg(
                        linear_gradient(
                            180.,
                            linear_color_stop(theme::panel(), 0.),
                            linear_color_stop(theme::panel().opacity(0.), 1.),
                        ),
                    ))
                }),
        )
    }

    fn render_message(&self, screen: &Screen, cx: &mut Context<Self>) -> Div {
        let mut card = MessageCard::default();

        match screen {
            Screen::Connecting => {
                card.badge = Badge::Spinner;
                card.color = theme::progress();
                card.title = "Connecting to playit".into();
                card.body =
                    vec!["Looking for the playit background service on this computer.".into()];
                card.waiting = Some("Connecting…".into());
            }
            Screen::Unreachable { error } => {
                card.badge = Badge::Icon(Icon::Power);
                card.color = theme::failure();
                let guidance = unreachable_guidance(self.state.session_mode, error);
                card.title = guidance.title.into();
                card.body = guidance.body;
                card.command = guidance.command;
                card.details = Some(format!("Address: {}\n{}", self.state.socket_path, error));
                card.waiting = Some("Reconnecting automatically every few seconds…".into());
                card.actions.push(
                    large(button(
                        "reconnect",
                        "Retry connection",
                        ButtonStyle::Secondary,
                        Some(Icon::Refresh),
                    ))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.send(UiCommand::Reconnect);
                        cx.notify();
                    }))
                    .into_any_element(),
                );
            }
            Screen::Starting => {
                card.badge = Badge::Icon(Icon::Clock);
                card.color = theme::progress();
                card.title = "Starting playit".into();
                card.body = vec!["This usually takes a few seconds.".into()];
                if self.state.lifecycle_since.elapsed() >= SLOW_START {
                    card.body.push(
                        "This is taking longer than usual. The activity log below shows what is happening.".into(),
                    );
                }
                card.waiting = Some("Waiting for the playit network…".into());
            }
            Screen::Stopping => {
                card.badge = Badge::Icon(Icon::Clock);
                card.color = theme::progress();
                card.title = "Stopping playit".into();
                card.body = vec!["Your tunnels are going offline.".into()];
                card.waiting = Some("Stopping…".into());
            }
            Screen::Error { message } => {
                let hint = error_hint(message);
                card.badge = Badge::Icon(Icon::XCircle);
                card.color = theme::failure();
                card.title = hint.title.into();
                card.body = vec![hint.next_step.to_string()];
                card.body.push(
                    "If it doesn't clear up on its own, the playit support team can help.".into(),
                );
                card.details = Some(message.to_string());
                card.waiting = Some("Retrying automatically…".into());
                card.actions.push(link_button(
                    "get-help",
                    "Get help",
                    SUPPORT_URL,
                    ButtonStyle::Secondary,
                ));
            }
            Screen::InvalidSecret { message } => {
                card.badge = Badge::Icon(Icon::Link);
                card.color = theme::attention();
                card.title = "Link this computer again".into();
                card.body = vec![
                    "The sign-in key saved on this computer no longer works. This computer may have been removed from your playit account.".into(),
                    "Linking it again takes a few seconds in your browser.".into(),
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
            Screen::OverLimit { message } => {
                card.badge = Badge::Icon(Icon::Alert);
                card.color = theme::attention();
                card.title = "Your account has too many computers linked".into();
                card.body = vec![
                    "Each computer you link uses one slot on your plan. Remove a computer you no longer use, or upgrade your plan.".into(),
                ];
                card.details = Some(message.to_string());
                card.waiting = Some("Starts automatically once a slot is free".into());
                card.actions.push(link_button(
                    "agents",
                    "Manage computers",
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
                        card.badge = Badge::Icon(Icon::XCircle);
                        card.color = theme::failure();
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
                                view.setup_link_opened = false;
                                view.send(UiCommand::StartSetup);
                                cx.notify();
                            }))
                            .into_any_element(),
                        );
                    }
                    step => {
                        card.steps = Some(setup_progress(step, self.setup_link_opened));
                        card.waiting = Some(
                            match step {
                                SetupStep::Preparing => "Preparing your setup link…",
                                SetupStep::Approved => "Approved. Finishing setup…",
                                _ => "Waiting for you to finish in your browser…",
                            }
                            .into(),
                        );
                        if let Some(url) = setup.url.clone() {
                            let open_url = url.clone();
                            card.actions.push(
                                large(button(
                                    "open-setup",
                                    "Open setup page",
                                    ButtonStyle::Primary,
                                    Some(Icon::External),
                                ))
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.setup_link_opened = true;
                                    cx.open_url(&open_url);
                                    cx.notify();
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

        // A fixed top offset keeps the badge and title in place as the state changes; only
        // the content below them grows.
        let mut group = div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(CARD_WIDTH))
            .max_w_full()
            .child(card.render(self.details_expanded, cx));
        if !self.state.logs.is_empty() {
            group = group.child(self.render_logs(false, cx));
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .pt(px(72.))
            .pb_4()
            .child(group)
    }

    fn render_footer(
        &self,
        screen: &Screen,
        more_below: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let left: AnyElement = if more_below {
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_color(theme::muted())
                .child(icon(Icon::ChevronDown, 14., theme::muted()))
                .child("Scroll for more tunnels, traffic, and activity")
                .into_any_element()
        } else if matches!(screen, Screen::Running(_)) {
            div()
                .min_w_0()
                .truncate()
                .child("Closing this window keeps playit and your tunnels running.")
                .into_any_element()
        } else {
            div().into_any_element()
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
                    .child(format!("Computer ID {short_id}"))
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
            .child(div().flex().min_w_0().child(left))
            .child(right)
    }
}

impl Render for PlayitWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = window.viewport_size().height < px(COMPACT_HEIGHT);
        let screen = self.state.screen();
        let body = match &screen {
            Screen::Running(agent) => self.render_running(agent, compact, cx),
            screen => self.render_message(screen, cx),
        };

        let mut root = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .font_family(UI_FONT)
            .text_color(theme::text())
            .text_sm()
            .child(self.render_header(&screen, compact, cx));

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
                    .text_color(theme::failure())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(icon(Icon::Alert, 16., theme::failure()))
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

        // The content scrolls only when the window is too short for it. A fade at the
        // bottom edge and a footer hint show that more is below.
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
                                .h(px(28.))
                                .bg(linear_gradient(
                                    180.,
                                    linear_color_stop(theme::bg().opacity(0.), 0.),
                                    linear_color_stop(theme::bg(), 1.),
                                )),
                        )
                    }),
            )
            .child(self.render_footer(&screen, more_below, cx));

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

enum NoticeAction {
    None,
    Link(String),
    SaveAccount,
}

fn notice_color(notice: &NoticeState) -> Hsla {
    match notice.priority.to_ascii_lowercase().as_str() {
        "high" | "critical" | "error" => theme::failure(),
        "low" | "info" => theme::progress(),
        _ => theme::attention(),
    }
}

fn notice_link(id: &'static str, label: &'static str, color: Hsla) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .text_color(color)
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|style| style.underline())
        .child(label)
        .child(icon(Icon::External, 13., color))
}

/// The account reads in neutral tones so green stays reserved for connection state.
fn render_account(status: &AccountStatus) -> AnyElement {
    let row = div().flex().items_center().gap_1p5();
    match status {
        AccountStatus::Verified => row
            .child(icon(Icon::CheckCircle, 14., theme::muted()))
            .child("Signed in"),
        AccountStatus::EmailNotVerified => row
            .text_color(theme::attention())
            .child(icon(Icon::Alert, 14., theme::attention()))
            .child("Verify your email"),
        AccountStatus::Guest => row
            .child(icon(Icon::User, 14., theme::muted()))
            .child("Guest"),
        AccountStatus::Unknown => row.text_color(theme::muted()).child("Unknown"),
    }
    .into_any_element()
}

struct ErrorHint {
    pill: &'static str,
    title: &'static str,
    next_step: &'static str,
}

/// Plain-language headings and next steps for agent errors with a recognizable cause.
fn error_hint(message: &str) -> ErrorHint {
    let message = message.to_ascii_lowercase();
    if message.contains("address already in use") || message.contains("address in use") {
        ErrorHint {
            pill: "Port in use",
            title: "Another program is blocking playit",
            next_step: "A network port playit needs is in use by another program. Close any second copy of playit, or restart your computer.",
        }
    } else if message.contains("permission denied") || message.contains("access is denied") {
        ErrorHint {
            pill: "Permission problem",
            title: "The playit service is missing a permission",
            next_step: "Restarting the playit service usually fixes this.",
        }
    } else if message.contains("timed out")
        || message.contains("dns")
        || message.contains("resolve")
    {
        ErrorHint {
            pill: "No network",
            title: "Can't reach the playit network",
            next_step: "Check your internet connection and firewall.",
        }
    } else {
        ErrorHint {
            pill: "Can't start",
            title: "Something interrupted playit",
            next_step: "Your tunnels are offline until it recovers.",
        }
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
    steps: Option<[StepState; 2]>,
    link: Option<String>,
    command: Option<String>,
    details: Option<String>,
    waiting: Option<SharedString>,
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
            waiting: None,
            actions: Vec::new(),
        }
    }
}

impl MessageCard {
    fn render(self, details_expanded: bool, cx: &mut Context<PlayitWindow>) -> impl IntoElement {
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
                    .w(px(CARD_WIDTH - CARD_PADDING_AND_BORDER))
                    .max_w_full()
                    .text_size(px(22.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .text_size(px(15.))
                    .text_color(theme::muted())
                    // A definite width keeps text measurement and painting in agreement;
                    // otherwise lines that wrap near the edge reserve an extra blank line.
                    .children(self.body.into_iter().map(|line| {
                        div()
                            .w(px(CARD_WIDTH - CARD_PADDING_AND_BORDER))
                            .max_w_full()
                            .child(line)
                    })),
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
                                    move |view, _, _, cx| {
                                        view.setup_link_opened = true;
                                        view.copy(copy.clone(), "link", cx)
                                    },
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
                    .child(spinner("waiting-spinner", 14., theme::progress()))
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
                    .pt_3()
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
                                    .id("toggle-details")
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .cursor_pointer()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::muted())
                                    .hover(|style| style.text_color(theme::text()))
                                    .child(icon(
                                        if details_expanded {
                                            Icon::ChevronDown
                                        } else {
                                            Icon::ChevronRight
                                        },
                                        14.,
                                        theme::muted(),
                                    ))
                                    .child("Technical details for support")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.details_expanded = !view.details_expanded;
                                        cx.notify();
                                    })),
                            )
                            .when(details_expanded, |header| {
                                header.child(
                                    button(
                                        "copy-details",
                                        "Copy error details",
                                        ButtonStyle::Secondary,
                                        Some(Icon::Copy),
                                    )
                                    .on_click(cx.listener(
                                        move |view, _, _, cx| {
                                            view.copy(copy.clone(), "error details", cx)
                                        },
                                    )),
                                )
                            }),
                    )
                    .when(details_expanded, |section| {
                        section.child(
                            inset()
                                .font_family(MONO_FONT)
                                .text_xs()
                                .text_color(theme::muted())
                                .child(details),
                        )
                    }),
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

/// The two things the user does; the waiting line under the card covers the rest.
fn setup_progress(step: &SetupStep, link_opened: bool) -> [StepState; 2] {
    use StepState::*;
    match step {
        SetupStep::Preparing => [Active, Todo],
        SetupStep::WaitingForUserVisit if link_opened => [Done, Active],
        SetupStep::WaitingForUserVisit => [Active, Todo],
        SetupStep::WaitingForUser => [Done, Active],
        SetupStep::Approved => [Done, Done],
        SetupStep::Rejected | SetupStep::Failed(_) => [Todo, Todo],
    }
}

fn render_steps(steps: [StepState; 2]) -> impl IntoElement {
    const LABELS: [&str; 2] = ["Open the setup page", "Sign in and approve this computer"];
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
                        StepState::Done => (
                            theme::online().opacity(0.16),
                            theme::online(),
                            theme::muted(),
                        ),
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
    title: &'static str,
    body: Vec<String>,
    command: Option<String>,
}

fn unreachable_guidance(mode: SessionMode, error: &str) -> UnreachableGuidance {
    if mode == SessionMode::ExternalDaemon {
        return UnreachableGuidance {
            title: "The playit background service isn't responding",
            body: vec![
                "This window shows the status of a separate playit background service, which isn't answering. Make sure that service is running. This window reconnects on its own.".into(),
            ],
            command: None,
        };
    }
    if cfg!(target_os = "linux") && error.contains("Permission denied") {
        return UnreachableGuidance {
            title: "Your user needs permission to use playit",
            body: vec![
                "The playit service is running, but your user isn't allowed to talk to it yet. Add yourself to the playit group, then log out and back in.".into(),
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
        title: "The playit service isn't running",
        body: vec![
            "Your tunnels are offline until the playit background service on this computer starts again.".into(),
            "Start it from a terminal. This window reconnects automatically:".into(),
        ],
        command: Some(command.into()),
    }
}

struct TunnelRow {
    dot_color: Hsla,
    address: String,
    dimmed: bool,
    /// Pending tunnels show their name, which isn't something to share yet.
    has_address: bool,
    /// Only exceptions get a label; the dot says "online" on its own.
    status: Option<(SharedString, Hsla)>,
    detail: Div,
    action: AnyElement,
}

/// One tunnel: status dot, the address with any exception label beside it, a detail line,
/// and an action in a fixed-width column so every row's button lines up.
fn tunnel_row(id: impl Into<ElementId>, compact: bool, row: TunnelRow) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .when(compact, |el| el.py_1p5())
        .when(!compact, |el| el.py_2())
        .border_b_1()
        .border_color(theme::border().opacity(0.6))
        .when(row.dimmed, |el| el.bg(theme::attention().opacity(0.04)))
        .hover(|style| style.bg(theme::panel_raised()))
        .child(dot(row.dot_color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(15.))
                                .when(row.has_address && !row.dimmed, |text| {
                                    text.font_weight(FontWeight::SEMIBOLD)
                                })
                                .text_color(if row.dimmed {
                                    theme::muted()
                                } else {
                                    theme::text()
                                })
                                .child(row.address),
                        )
                        .children(row.status.map(|(label, color)| chip(label, color))),
                )
                .child(div().flex().min_w_0().text_size(px(13.)).child(row.detail)),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .w(px(ROW_ACTION_WIDTH))
                .child(row.action),
        )
}

fn log_row(entry: &LogEntry, today: chrono::NaiveDate, show_target: bool) -> Div {
    let (level_color, message_color) = match entry.level {
        LogLevel::Error => (theme::failure(), theme::text()),
        LogLevel::Warn => (theme::attention(), theme::text()),
        LogLevel::Info => (theme::faint(), theme::muted()),
        LogLevel::Debug | LogLevel::Trace => (theme::faint(), theme::faint()),
    };
    div()
        .w_full()
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
            div().flex_none().w(px(50.)).child(
                div()
                    .flex()
                    .justify_center()
                    .w(px(44.))
                    .rounded_sm()
                    .when(
                        matches!(entry.level, LogLevel::Error | LogLevel::Warn),
                        |chip| chip.bg(level_color.opacity(0.16)),
                    )
                    .text_color(level_color)
                    .child(level_label(&entry.level)),
            ),
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

/// Describes where a tunnel forwards to, naming this computer instead of a loopback address.
fn destination_label(destination: &str) -> String {
    let (host, port) = match destination.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host.trim_matches(['[', ']']), Some(port))
        }
        _ => (destination, None),
    };
    let loopback = host == "localhost" || host == "::1" || host.starts_with("127.");
    match (loopback, port) {
        (true, Some(port)) => format!("this computer, port {port}"),
        (true, None) => "this computer".to_string(),
        (false, _) => destination.to_string(),
    }
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

fn chip(label: impl Into<SharedString>, color: Hsla) -> impl IntoElement {
    div()
        .flex_none()
        .px_1p5()
        .rounded_sm()
        .bg(color.opacity(0.14))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(label.into())
}

/// Logged problem counts in the log header, kept soft so they read as history.
fn count_badge(label: String, color: Hsla) -> impl IntoElement {
    div()
        .px_1p5()
        .rounded_sm()
        .border_1()
        .border_color(color.opacity(0.2))
        .bg(color.opacity(0.07))
        .text_color(color)
        .font_weight(FontWeight::MEDIUM)
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
    caption: Option<String>,
    divider: bool,
) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .px_4()
        .pt_1()
        .pb_2p5()
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
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value),
                )
                .children(caption.map(|caption| {
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme::muted())
                        .child(caption)
                })),
        )
}

fn toggle(id: &'static str, label: impl Into<SharedString>, on: bool) -> Stateful<Div> {
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
        .child(label.into())
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

/// A full-width, left-aligned row action, so every row's icon and label start at the same x.
fn row_button(id: impl Into<ElementId>, label: &'static str, kind: Icon) -> Stateful<Div> {
    button(id, label, ButtonStyle::Secondary, Some(kind))
        .w_full()
        .px_3()
        .py_1p5()
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

#[cfg(test)]
mod tests {
    use super::destination_label;

    #[test]
    fn loopback_destinations_name_this_computer() {
        assert_eq!(
            destination_label("127.0.0.1:25565"),
            "this computer, port 25565"
        );
        assert_eq!(destination_label("[::1]:8080"), "this computer, port 8080");
        assert_eq!(destination_label("localhost"), "this computer");
        assert_eq!(
            destination_label("192.168.1.20:19132"),
            "192.168.1.20:19132"
        );
    }
}
