use std::time::{Duration, Instant};

use chrono::Local;
use gpui::{
    AnyElement, ClipboardItem, Context, Div, ElementId, FontWeight, Hsla, ScrollStrategy,
    SharedString, Stateful, Task, UniformListScrollHandle, Window, div, prelude::*, px, rgb,
    uniform_list,
};
use playit_ipc::model::{AccountStatus, AgentState, LogLevel};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::backend::{BackendEvent, CommandOutcome, SetupStep, UiCommand};
use crate::state::{
    ACCOUNT_AGENTS_URL, ACCOUNT_UPGRADE_URL, AppState, Screen, SessionMode, disabled_label,
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
        rgb(0x232331).into()
    }
    pub fn border() -> Hsla {
        rgb(0x2d2d3d).into()
    }
    pub fn text() -> Hsla {
        rgb(0xe8e8f0).into()
    }
    pub fn muted() -> Hsla {
        rgb(0x8d8da3).into()
    }
    pub fn faint() -> Hsla {
        rgb(0x5f5f75).into()
    }
    pub fn accent() -> Hsla {
        rgb(0xd17bf7).into()
    }
    pub fn accent_strong() -> Hsla {
        rgb(0x9d4edd).into()
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
    pub fn cyan() -> Hsla {
        rgb(0x22d3ee).into()
    }
}

#[cfg(target_os = "macos")]
const MONO_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
const MONO_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO_FONT: &str = "DejaVu Sans Mono";

const LOG_ROW_HEIGHT: f32 = 20.;
const TOAST_DURATION: Duration = Duration::from_secs(2);

pub struct PlayitWindow {
    state: AppState,
    commands: UnboundedSender<UiCommand>,
    log_scroll: UniformListScrollHandle,
    follow_logs: bool,
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
        if self.follow_logs && !self.state.logs.is_empty() {
            self.log_scroll
                .scroll_to_item(self.state.logs.len() - 1, ScrollStrategy::Bottom);
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

    fn render_header(&self, screen: &Screen, cx: &mut Context<Self>) -> impl IntoElement {
        let (status_label, status_color) = match screen {
            Screen::Running(_) => ("Agent running", theme::green()),
            Screen::Connecting | Screen::Starting => ("Connecting", theme::yellow()),
            Screen::Stopping => ("Stopping", theme::yellow()),
            Screen::Setup(_) => ("Setup required", theme::blue()),
            Screen::Unreachable { .. } => ("Service unreachable", theme::red()),
            Screen::InvalidSecret { .. } | Screen::OverLimit | Screen::Error { .. } => {
                ("Needs attention", theme::red())
            }
        };
        let version = match screen {
            Screen::Running(agent) if !agent.version.is_empty() => agent.version.clone(),
            _ => env!("CARGO_PKG_VERSION").to_string(),
        };

        let mut right = div().flex().items_center().gap_3();
        if let Screen::Running(agent) = screen {
            let now = playit_now_millis();
            right = right
                .child(header_stat(
                    "Uptime",
                    div().child(format_uptime(agent.start_time, now)),
                ))
                .child(header_stat(
                    "Account",
                    account_status(&agent.account_status),
                ))
                .child(
                    button("dashboard", "Open dashboard", ButtonStyle::Secondary).on_click(
                        cx.listener(|view, _, _, cx| {
                            view.send(UiCommand::OpenDashboard);
                            cx.notify();
                        }),
                    ),
                );
        }

        div()
            .flex()
            .items_center()
            .justify_between()
            .px_5()
            .py_3()
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
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme::accent())
                                    .child("playit"),
                            )
                            .child(
                                div()
                                    .text_color(theme::muted())
                                    .child(format!("v{version}")),
                            ),
                    )
                    .child(pill(status_label, status_color)),
            )
            .child(right)
    }

    fn render_running(&self, agent: &AgentState, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col().gap_3().flex_1().min_h_0();

        for (ix, notice) in agent.notices.iter().enumerate() {
            let color = match notice.priority.to_ascii_lowercase().as_str() {
                "high" | "critical" | "error" => theme::red(),
                _ => theme::yellow(),
            };
            let mut banner = panel()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_2()
                .border_color(color)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(dot(color))
                        .child(notice.message.clone()),
                );
            if let Some(link) = notice.resolve_link.clone() {
                banner = banner.child(
                    button(("notice", ix), "Resolve", ButtonStyle::Secondary)
                        .on_click(move |_, _, cx| cx.open_url(&link)),
                );
            }
            body = body.child(banner);
        }

        body.child(self.render_tunnels(agent, cx))
            .child(self.render_stats())
            .child(self.render_logs(cx))
    }

    fn render_tunnels(&self, agent: &AgentState, cx: &mut Context<Self>) -> impl IntoElement {
        let mut header_right = div().flex().items_center().gap_2();
        if !agent.agent_id.is_empty() {
            let agent_id = agent.agent_id.clone();
            header_right = header_right.child(
                div()
                    .id("agent-id")
                    .flex()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::panel_raised()))
                    .text_xs()
                    .child(div().text_color(theme::faint()).child("Agent ID"))
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_color(theme::muted())
                            .child(agent.agent_id.clone()),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.copy(agent_id.clone(), "agent ID", cx)
                    })),
            );
        }

        let mut list = div()
            .id("tunnel-list")
            .flex()
            .flex_col()
            .p_2()
            .max_h(px(260.))
            .overflow_y_scroll();
        if agent.tunnels.is_empty() && agent.pending_tunnels.is_empty() {
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .py_4()
                    .text_color(theme::muted())
                    .child("No tunnels configured yet.")
                    .child(
                        button(
                            "add-tunnel",
                            "Add a tunnel on playit.gg",
                            ButtonStyle::Primary,
                        )
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.send(UiCommand::OpenDashboard);
                            cx.notify();
                        })),
                    ),
            );
        }

        for (ix, tunnel) in agent.tunnels.iter().enumerate() {
            let (color, detail) = if tunnel.is_disabled {
                (
                    theme::yellow(),
                    disabled_label(tunnel.disabled_reason.as_deref()),
                )
            } else {
                (theme::green(), format!("→  {}", tunnel.destination))
            };
            let address = tunnel.display_address.clone();
            list = list.child(
                div()
                    .id(("tunnel", ix))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .hover(|style| style.bg(theme::panel_raised()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .min_w_0()
                            .child(dot(color))
                            .child(
                                div()
                                    .font_family(MONO_FONT)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(if tunnel.is_disabled {
                                        theme::muted()
                                    } else {
                                        theme::text()
                                    })
                                    .child(tunnel.display_address.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_family(MONO_FONT)
                                    .text_color(if tunnel.is_disabled {
                                        theme::yellow()
                                    } else {
                                        theme::muted()
                                    })
                                    .child(detail),
                            ),
                    )
                    .child(
                        button(("copy-tunnel", ix), "Copy address", ButtonStyle::Ghost).on_click(
                            cx.listener(move |view, _, _, cx| {
                                view.copy(address.clone(), "address", cx)
                            }),
                        ),
                    ),
            );
        }

        for pending in &agent.pending_tunnels {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_1p5()
                    .child(dot(theme::yellow()))
                    .child(div().font_family(MONO_FONT).child(pending.id.clone()))
                    .child(
                        div()
                            .text_color(theme::yellow())
                            .child(pending.status_msg.clone()),
                    ),
            );
        }

        panel()
            .flex()
            .flex_col()
            .flex_none()
            .child(panel_header(
                "Tunnels",
                Some(header_right.into_any_element()),
            ))
            .child(list)
    }

    fn render_stats(&self) -> impl IntoElement {
        let stats = &self.state.stats;
        div()
            .flex_none()
            .flex()
            .gap_3()
            .child(stat_tile(
                "Data in",
                format_bytes(stats.bytes_in),
                theme::green(),
            ))
            .child(stat_tile(
                "Data out",
                format_bytes(stats.bytes_out),
                theme::blue(),
            ))
            .child(stat_tile(
                "TCP connections",
                stats.active_tcp.to_string(),
                theme::cyan(),
            ))
            .child(stat_tile(
                "UDP flows",
                stats.active_udp.to_string(),
                theme::accent(),
            ))
    }

    fn render_logs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.state.logs.len();
        let follow_label = if self.follow_logs {
            "Following"
        } else {
            "Paused"
        };

        let header_right = div()
            .flex()
            .gap_2()
            .child(
                button(
                    "follow-logs",
                    follow_label,
                    if self.follow_logs {
                        ButtonStyle::Secondary
                    } else {
                        ButtonStyle::Ghost
                    },
                )
                .on_click(cx.listener(|view, _, _, cx| {
                    view.follow_logs = !view.follow_logs;
                    view.after_state_change();
                    cx.notify();
                })),
            )
            .child(
                button("copy-logs", "Copy logs", ButtonStyle::Ghost).on_click(cx.listener(
                    |view, _, _, cx| {
                        let text = view.state.logs_text();
                        view.copy(text, "logs", cx)
                    },
                )),
            );

        let content: AnyElement = if count == 0 {
            div()
                .p_4()
                .text_color(theme::faint())
                .child("No log messages yet.")
                .into_any_element()
        } else {
            uniform_list(
                "logs",
                count,
                cx.processor(|view, range: std::ops::Range<usize>, _window, _cx| {
                    let today = Local::now().date_naive();
                    view.state
                        .logs
                        .range(range)
                        .map(|entry| {
                            let level_color = match entry.level {
                                LogLevel::Error => theme::red(),
                                LogLevel::Warn => theme::yellow(),
                                LogLevel::Info => theme::green(),
                                LogLevel::Debug => theme::blue(),
                                LogLevel::Trace => theme::muted(),
                            };
                            div()
                                .h(px(LOG_ROW_HEIGHT))
                                .px_3()
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
                                        .w(px(48.))
                                        .text_color(level_color)
                                        .child(level_label(&entry.level)),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(theme::muted())
                                        .child(format!("{}:", short_target(&entry.target))),
                                )
                                .child(div().truncate().child(entry.message.clone()))
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.log_scroll.clone())
            .flex_1()
            .py_1()
            .into_any_element()
        };

        panel()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(140.))
            .child(panel_header(
                format!("Agent logs ({count})"),
                Some(header_right.into_any_element()),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .font_family(MONO_FONT)
                    .text_xs()
                    .child(content),
            )
    }

    fn render_message(&self, screen: &Screen, cx: &mut Context<Self>) -> Div {
        let socket_path = self.state.socket_path.as_str();
        let mut actions: Vec<AnyElement> = Vec::new();
        let mut url_box: Option<String> = None;

        let (color, title, lines): (Hsla, &str, Vec<String>) = match screen {
            Screen::Connecting => (
                theme::yellow(),
                "Connecting to playit…",
                vec![format!("Looking for the playit service at {socket_path}")],
            ),
            Screen::Unreachable { error } => (
                theme::red(),
                "Can't reach the playit service",
                unreachable_guidance(self.state.session_mode, socket_path, error),
            ),
            Screen::Starting => (
                theme::yellow(),
                "The playit agent is connecting…",
                vec!["This usually takes a few seconds.".to_string()],
            ),
            Screen::Stopping => (theme::yellow(), "The playit agent is stopping…", vec![]),
            Screen::Error { message } => (
                theme::red(),
                "The playit agent reported an error",
                vec![message.to_string()],
            ),
            Screen::InvalidSecret { message } => {
                actions.push(
                    button(
                        "reset-secret",
                        "Reset and set up again",
                        ButtonStyle::Primary,
                    )
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.command_error = None;
                        view.send(UiCommand::ResetSecret);
                        cx.notify();
                    }))
                    .into_any_element(),
                );
                (
                    theme::red(),
                    "The playit agent has an invalid secret",
                    vec![
                        message.to_string(),
                        "Resetting removes the saved secret so you can link this agent again."
                            .to_string(),
                    ],
                )
            }
            Screen::OverLimit => {
                actions.push(link_button(
                    "agents",
                    "Delete unused agents",
                    ACCOUNT_AGENTS_URL,
                    ButtonStyle::Primary,
                ));
                actions.push(link_button(
                    "upgrade",
                    "Increase your agent limit",
                    ACCOUNT_UPGRADE_URL,
                    ButtonStyle::Secondary,
                ));
                (
                    theme::red(),
                    "This account is over the agent limit",
                    vec!["The playit agent cannot start until an agent slot is free.".to_string()],
                )
            }
            Screen::Setup(setup) => {
                let (color, line) = match &setup.step {
                    SetupStep::Preparing => (theme::blue(), "Preparing setup…".to_string()),
                    SetupStep::WaitingForUserVisit => (
                        theme::blue(),
                        "Open the setup page to link this agent to your playit account."
                            .to_string(),
                    ),
                    SetupStep::WaitingForUser => (
                        theme::blue(),
                        "Approve this program in your browser.".to_string(),
                    ),
                    SetupStep::Approved => (
                        theme::green(),
                        "Program approved. Finishing setup…".to_string(),
                    ),
                    SetupStep::Rejected => (
                        theme::red(),
                        "Setup was not approved in the browser.".to_string(),
                    ),
                    SetupStep::Failed(error) => (theme::red(), error.clone()),
                };

                if matches!(setup.step, SetupStep::Rejected | SetupStep::Failed(_)) {
                    actions.push(
                        button("retry-setup", "Try again", ButtonStyle::Primary)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.state.begin_setup();
                                view.send(UiCommand::StartSetup);
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                } else if let Some(url) = setup.url.clone() {
                    let copy_url = url.clone();
                    actions.push(link_button(
                        "open-setup",
                        "Open setup page",
                        &url,
                        ButtonStyle::Primary,
                    ));
                    actions.push(
                        button("copy-setup", "Copy link", ButtonStyle::Secondary)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.copy(copy_url.clone(), "link", cx)
                            }))
                            .into_any_element(),
                    );
                    url_box = Some(url);
                }
                (color, "Set up playit", vec![line])
            }
            Screen::Running(_) => unreachable!("running screen has its own layout"),
        };

        let mut card = panel()
            .flex()
            .flex_col()
            .gap_3()
            .p_6()
            .w_full()
            .max_w(px(620.))
            .child(
                div().flex().items_center().gap_3().child(dot(color)).child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_color(theme::muted())
                    .children(lines.into_iter().map(|line| {
                        if line.starts_with("  ") {
                            div()
                                .font_family(MONO_FONT)
                                .text_color(theme::text())
                                .pl_4()
                                .child(line.trim_start().to_string())
                        } else {
                            div().child(line)
                        }
                    })),
            );
        if let Some(url) = url_box {
            card = card.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(theme::bg())
                    .border_1()
                    .border_color(theme::border())
                    .font_family(MONO_FONT)
                    .text_color(theme::cyan())
                    .child(url),
            );
        }
        if !actions.is_empty() {
            card = card.child(div().flex().gap_2().pt_1().children(actions));
        }

        let mut body = div().flex().flex_col().gap_3().flex_1().min_h_0().child(
            div()
                .flex_none()
                .flex()
                .justify_center()
                .pt_6()
                .pb_2()
                .child(card),
        );
        if !self.state.logs.is_empty() {
            body = body.child(self.render_logs(cx));
        }
        body
    }

    fn render_footer(&self) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px_5()
            .py_2()
            .border_t_1()
            .border_color(theme::border())
            .text_xs()
            .text_color(theme::muted())
            .child(
                div()
                    .flex_none()
                    .child(self.state.session_mode.description()),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO_FONT)
                    .text_color(theme::faint())
                    .child(self.state.socket_path.clone()),
            )
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
                    .items_center()
                    .justify_between()
                    .px_5()
                    .py_2()
                    .bg(rgb(0x3a1d24))
                    .text_color(theme::red())
                    .child(error)
                    .child(
                        button("dismiss-error", "Dismiss", ButtonStyle::Ghost).on_click(
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
            .child(self.render_footer());

        if let Some((toast, _)) = &self.toast {
            root = root.child(
                div()
                    .absolute()
                    .bottom(px(44.))
                    .right(px(20.))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(theme::accent_strong())
                    .text_color(rgb(0xffffff))
                    .shadow_lg()
                    .child(toast.clone()),
            );
        }
        root
    }
}

fn unreachable_guidance(mode: SessionMode, socket_path: &str, error: &str) -> Vec<String> {
    let mut lines = Vec::new();
    if mode == SessionMode::ExternalDaemon {
        lines.push(format!(
            "No playit daemon is answering at {socket_path}. Start it and this window will connect."
        ));
    } else if cfg!(target_os = "linux") && error.contains("Permission denied") {
        lines.push(
            "The playit service is running, but your user cannot access its socket. Add your user to the playit group, then log out and back in:".to_string(),
        );
        lines.push("  sudo usermod -aG playit $USER".to_string());
    } else {
        lines.push("The background service isn't running. Start it from a terminal:".to_string());
        // `playit start` on Linux needs to know the init system, so name systemd directly.
        let command = if cfg!(target_os = "linux") {
            "  sudo systemctl start playit"
        } else {
            "  playit start"
        };
        lines.push(command.to_string());
    }
    lines.push(format!("Details: {error}"));
    lines.push("Retrying automatically…".to_string());
    lines
}

fn playit_now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

fn panel() -> Div {
    div()
        .bg(theme::panel())
        .border_1()
        .border_color(theme::border())
        .rounded_lg()
        .overflow_hidden()
}

fn panel_header(title: impl Into<SharedString>, right: Option<AnyElement>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .h(px(40.))
        .px_4()
        .border_b_1()
        .border_color(theme::border())
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title.into()))
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
        .text_color(color)
        .child(dot(color))
        .child(label)
}

fn header_stat(label: &'static str, value: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_end()
        .px_2()
        .child(div().text_xs().text_color(theme::faint()).child(label))
        .child(value)
}

fn account_status(status: &AccountStatus) -> impl IntoElement {
    let (label, color) = match status {
        AccountStatus::Verified => ("Email verified", theme::green()),
        AccountStatus::Guest => ("Guest", theme::yellow()),
        AccountStatus::EmailNotVerified => ("Email not verified", theme::yellow()),
        AccountStatus::Unknown => ("Unknown", theme::muted()),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_color(color)
        .child(dot(color))
        .child(label)
}

fn stat_tile(label: &'static str, value: String, color: Hsla) -> impl IntoElement {
    panel()
        .flex_1()
        .flex()
        .flex_col()
        .gap_1()
        .px_4()
        .py_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme::muted())
                .child(dot(color))
                .child(label),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
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
) -> Stateful<Div> {
    let (bg, hover_bg, border, text) = match style {
        ButtonStyle::Primary => (
            theme::accent_strong(),
            rgb(0xb064f0).into(),
            theme::accent_strong(),
            rgb(0xffffff).into(),
        ),
        ButtonStyle::Secondary => (
            theme::panel_raised(),
            rgb(0x2e2e40).into(),
            theme::border(),
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
        .flex_none()
        .px_3()
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
        .child(label.into())
}

fn link_button(id: &'static str, label: &'static str, url: &str, style: ButtonStyle) -> AnyElement {
    let url = url.to_string();
    button(id, label, style)
        .on_click(move |_, _, cx| cx.open_url(&url))
        .into_any_element()
}
