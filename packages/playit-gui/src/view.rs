use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::Local;
use gpui::{
    Animation, AnimationExt, AnyElement, Bounds, ClipboardItem, Context, CursorStyle, Decorations,
    Div, ElementId, FontWeight, HitboxBehavior, Hsla, ImageSource, MouseButton, Pixels, Point,
    ResizeEdge, Resource, ScrollHandle, ScrollStrategy, SharedString, Size, Stateful, Svg, Task,
    Tiling, Transformation, UniformListScrollHandle, Window, WindowControls, canvas, div, img,
    linear_color_stop, linear_gradient, percentage, point, prelude::*, px, svg, uniform_list,
};
use playit_ipc::model::{AccountStatus, AgentState, LogEntry, LogLevel, NoticeState};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::assets::{Icon, LOGO_MARK, LOGO_WORDMARK};
use crate::backend::{BackendEvent, CommandOutcome, START_NEEDS_PASSWORD, SetupStep, UiCommand};
use crate::state::{
    ACCOUNT_AGENTS_URL, ACCOUNT_UPGRADE_URL, ACCOUNT_URL, AppState, Screen, SessionMode,
    disabled_reason, format_bytes, format_uptime, level_label, log_time_label, new_tunnel_url,
    short_target, tunnel_url,
};

const SUPPORT_URL: &str = "https://playit.gg/support";

/// Colors carry one meaning each: green is online, cyan is in progress, orange needs
/// attention and marks the primary action, red means something failed. The values are
/// playit.gg's website tokens.
mod theme {
    use gpui::{Hsla, rgb};

    /// A status color with the tints the website uses for fills and borders.
    #[derive(Clone, Copy)]
    pub struct Tone {
        pub text: Hsla,
        pub strong: Hsla,
        pub surface: Hsla,
        pub border: Hsla,
        pub subtle: Hsla,
    }

    fn tone(text: u32, strong: u32, surface: u32, border: u32, subtle: u32) -> Tone {
        Tone {
            text: rgb(text).into(),
            strong: rgb(strong).into(),
            surface: rgb(surface).into(),
            border: rgb(border).into(),
            subtle: rgb(subtle).into(),
        }
    }

    pub fn success() -> Tone {
        tone(0x5fd394, 0x16a34a, 0x0d412f, 0x0e5234, 0x0c362c)
    }
    pub fn warning() -> Tone {
        tone(0xf7a560, 0xff6b00, 0x533019, 0x783d14, 0x3f291c)
    }
    pub fn danger() -> Tone {
        tone(0xff8a8a, 0xdc2626, 0x4d1c25, 0x922125, 0x3c1b24)
    }
    pub fn info() -> Tone {
        tone(0x22ccf5, 0x22ccf5, 0x0c3041, 0x135f78, 0x0c2d3d)
    }
    pub fn brand() -> Tone {
        tone(0xff8533, 0xff6b00, 0x3a281d, 0x783d14, 0x35261e)
    }
    pub fn neutral() -> Tone {
        tone(0xbccbe0, 0x93a8c4, 0x0c2340, 0x1f4f8f, 0x0b1d33)
    }

    pub fn bg() -> Hsla {
        rgb(0x091724).into()
    }
    pub fn header_band() -> Hsla {
        rgb(0x0c2340).into()
    }
    pub fn panel() -> Hsla {
        rgb(0x0b1d33).into()
    }
    pub fn interactive() -> Hsla {
        rgb(0x0f2a4b).into()
    }
    pub fn inset() -> Hsla {
        rgb(0x0a1c33).into()
    }
    pub fn border_muted() -> Hsla {
        rgb(0x112c52).into()
    }
    pub fn border() -> Hsla {
        rgb(0x16376c).into()
    }
    pub fn border_strong() -> Hsla {
        rgb(0x1f4f8f).into()
    }
    pub fn text() -> Hsla {
        rgb(0xeef3fa).into()
    }
    pub fn muted() -> Hsla {
        rgb(0xbccbe0).into()
    }
    pub fn faint() -> Hsla {
        rgb(0x93a8c4).into()
    }
    pub fn placeholder() -> Hsla {
        rgb(0x7f95b5).into()
    }
    pub fn primary() -> Hsla {
        rgb(0xff6b00).into()
    }
    pub fn primary_hover() -> Hsla {
        rgb(0xe05e00).into()
    }
    pub fn primary_border() -> Hsla {
        rgb(0xd45d08).into()
    }
    /// Orange for text and icons, lighter than `primary` so it reads on navy.
    pub fn primary_text() -> Hsla {
        rgb(0xff8533).into()
    }
    pub fn on_primary() -> Hsla {
        rgb(0xffffff).into()
    }
    pub fn secondary() -> Hsla {
        rgb(0x3b82f6).into()
    }
    pub fn secondary_hover_bg() -> Hsla {
        rgb(0x173664).into()
    }
    pub fn link() -> Hsla {
        rgb(0x22ccf5).into()
    }
    pub fn link_hover() -> Hsla {
        rgb(0x64dbf8).into()
    }
    pub fn online() -> Hsla {
        rgb(0x5fd394).into()
    }
    pub fn online_strong() -> Hsla {
        rgb(0x16a34a).into()
    }
    pub fn progress() -> Hsla {
        rgb(0x22ccf5).into()
    }
    pub fn attention() -> Hsla {
        rgb(0xf7a560).into()
    }
    pub fn attention_strong() -> Hsla {
        rgb(0xff6b00).into()
    }
    pub fn failure() -> Hsla {
        rgb(0xff8a8a).into()
    }
    pub fn failure_strong() -> Hsla {
        rgb(0xdc2626).into()
    }
}

use theme::Tone;

/// Corner radius of buttons, panels, chips, code boxes and callouts.
const RADIUS: f32 = 6.;
/// Corner radius of ghost buttons, the toast and the message card.
const RADIUS_SM: f32 = 8.;

/// Inter, bundled and registered at startup, as on playit.gg.
const UI_FONT: &str = "Inter";

/// The monospace font for commands, addresses and logs, chosen by `init_mono_font`.
static MONO_FONT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Monospace fonts to use, in order of preference. macOS and Windows always have theirs; Linux
/// installs vary, and GPUI panics when neither a font nor its fallbacks are installed.
#[cfg(target_os = "macos")]
const MONO_FONT_CANDIDATES: &[&str] = &["Menlo"];
#[cfg(target_os = "windows")]
const MONO_FONT_CANDIDATES: &[&str] = &["Consolas"];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO_FONT_CANDIDATES: &[&str] = &[
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Ubuntu Mono",
];

/// Picks the first installed monospace font, or the bundled UI font when none is installed.
pub fn init_mono_font(cx: &gpui::App) {
    let installed = cx.text_system().all_font_names();
    let font = MONO_FONT_CANDIDATES
        .iter()
        .copied()
        .find(|candidate| installed.iter().any(|name| name == candidate))
        .unwrap_or(UI_FONT);
    let _ = MONO_FONT.set(font);
}

fn mono_font() -> &'static str {
    MONO_FONT.get().copied().unwrap_or(UI_FONT)
}

const LOG_ROW_HEIGHT: f32 = 22.;
/// An expanded log shows at most this many lines before it scrolls.
const LOG_MAX_ROWS: usize = 8;
const CARD_WIDTH: f32 = 580.;
const SECTION_TITLE_SIZE: f32 = 17.;
/// `p_8` on both sides plus the 1px border.
const CARD_PADDING_AND_BORDER: f32 = 66.;
/// Width of the action column on tunnel rows, so every row's button lines up.
const ROW_ACTION_WIDTH: f32 = 146.;
const TOAST_DURATION: Duration = Duration::from_secs(2);
/// Starting longer than this gets a "taking longer than usual" hint.
const SLOW_START: Duration = Duration::from_secs(20);
/// Windows shorter than this use tighter spacing.
const COMPACT_HEIGHT: f32 = 640.;
/// Windows narrower than this drop the account summary and shorten long lines.
const NARROW_WIDTH: f32 = 760.;

const GUEST_NOTICE: &str = "You're using a guest account. Save it to keep your tunnels if you reinstall or switch computers.";

pub struct PlayitWindow {
    state: AppState,
    commands: UnboundedSender<UiCommand>,
    content_scroll: ScrollHandle,
    /// Where the first disabled tunnel was last painted, so its count can scroll to it.
    attention_row: Rc<Cell<Option<Bounds<Pixels>>>>,
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
            attention_row: Rc::default(),
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

    /// Website links assume the browser is already signed in; only guest accounts need the
    /// daemon's one-click login link, which `login_link` carries when the daemon has one.
    fn open_guest_login(&mut self, login_link: Option<String>, cx: &mut Context<Self>) {
        match login_link {
            Some(link) => cx.open_url(&link),
            None => self.send(UiCommand::OpenGuestLogin),
        }
        cx.notify();
    }

    fn copy(&mut self, text: String, label: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.toast = Some((format!("Copied {label}").into(), Instant::now()));
        cx.notify();
    }

    /// Scrolls the main view so the first disabled tunnel sits near the top.
    fn scroll_to_attention(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.attention_row.get() else {
            return;
        };
        let viewport = self.content_scroll.bounds();
        let offset = self.content_scroll.offset();
        let max = self.content_scroll.max_offset().height;
        let target = (offset.y - (row.top() - viewport.top() - px(8.))).clamp(-max, px(0.));
        self.content_scroll.set_offset(point(offset.x, target));
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
        narrow: bool,
        chrome: Option<WindowChrome>,
    ) -> impl IntoElement {
        let (status_label, status_tone) = match screen {
            Screen::Running(_) => ("Connected to playit", theme::success()),
            Screen::Connecting => ("Connecting", theme::info()),
            Screen::StartingService => ("Starting", theme::info()),
            Screen::Starting => ("Starting", theme::info()),
            Screen::Stopping => ("Stopping", theme::info()),
            Screen::Setup(_) => ("Not set up", theme::neutral()),
            Screen::Unreachable { .. } => ("Can't connect", theme::danger()),
            // Whenever tunnels can't carry traffic the badge says so; the card says why and
            // whether playit is retrying on its own.
            Screen::InvalidSecret { .. } | Screen::OverLimit { .. } | Screen::Error { .. } => {
                ("Tunnels offline", theme::warning())
            }
        };

        let mut right = div().flex().items_center().gap_5();
        if let Screen::Running(agent) = screen {
            right = right
                .child(header_stat(
                    "Running for",
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
                        if narrow {
                            "Dashboard"
                        } else {
                            "Open dashboard"
                        },
                        ButtonStyle::Secondary,
                        Some(Icon::External),
                    )
                    .on_click(|_, _, cx| cx.open_url(ACCOUNT_URL)),
                );
        }
        if let Some(chrome) = chrome {
            right = right.child(window_controls(chrome));
        }

        // Without OS decorations the header doubles as the title bar: it moves the window,
        // double-click maximizes, right-click opens the window menu, and the controls sit at
        // the far right.
        div()
            .id("titlebar")
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .pl_5()
            .when(chrome.is_some(), |header| header.pr_2())
            .when(chrome.is_none(), |header| header.pr_5())
            .h(px(if compact { 56. } else { 64. }))
            .border_b_1()
            .border_color(theme::border())
            .bg(theme::header_band())
            .when(chrome.is_some(), |header| {
                header
                    .on_mouse_down(MouseButton::Left, |event, window, _| {
                        if event.click_count == 1 {
                            window.start_window_move();
                        }
                    })
                    .on_click(|event, window, _| {
                        if event.is_right_click() {
                            window.show_window_menu(event.position());
                        } else if event.click_count() == 2 {
                            window.zoom_window();
                        }
                    })
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    // The website's pixel-art wordmark, or just its computer when narrow. Both
                    // are drawn at exactly half their bundled size so the pixels stay crisp.
                    .child(if narrow {
                        img(ImageSource::Resource(Resource::Embedded(LOGO_MARK.into())))
                            .size(px(32.))
                            .flex_none()
                    } else {
                        img(ImageSource::Resource(Resource::Embedded(
                            LOGO_WORDMARK.into(),
                        )))
                        .w(px(168.))
                        .h(px(32.))
                        .flex_none()
                    })
                    .child(pill(status_label, status_tone)),
            )
            .child(right)
    }

    fn render_running(
        &self,
        agent: &AgentState,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> Div {
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
                // Losing a guest account loses its tunnels, so this outranks routine notices.
                theme::warning(),
                NoticeAction::GuestLogin(agent.login_link.clone()),
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
                notice_tone(notice),
                action,
                compact,
                cx,
            ));
        }

        if !has_tunnels {
            return body
                .child(self.render_no_tunnels(&agent.agent_id))
                .child(self.render_logs(true, cx));
        }
        body.child(self.render_tunnels(agent, compact, narrow, cx))
            .child(self.render_stats())
            .child(self.render_logs(true, cx))
    }

    fn render_notice(
        &self,
        id: impl Into<ElementId>,
        message: &str,
        tone: Tone,
        action: NoticeAction,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let dismissed = message.to_string();
        let action: Option<AnyElement> =
            match action {
                NoticeAction::None => None,
                NoticeAction::Link(link) => Some(
                    button(
                        "notice-link",
                        "See what's new",
                        ButtonStyle::Ghost,
                        Some(Icon::External),
                    )
                    .on_click(move |_, _, cx| cx.open_url(&link))
                    .into_any_element(),
                ),
                NoticeAction::GuestLogin(login_link) => Some(
                    button(
                        "guest-login",
                        "Save account",
                        // The guest notice is a warning band, so its action matches it.
                        ButtonStyle::Warning,
                        Some(Icon::External),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.open_guest_login(login_link.clone(), cx)
                    }))
                    .into_any_element(),
                ),
            };
        div()
            .id(id.into())
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .pr_3()
            .pl(px(13.))
            .when(compact, |notice| notice.py_1())
            .when(!compact, |notice| notice.py_1p5())
            .rounded(px(RADIUS))
            .border_l(px(4.))
            .border_color(tone.strong)
            .bg(tone.surface)
            .child(icon(Icon::Info, 15., tone.text))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text())
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
                    .hover(move |style| style.bg(tone.border))
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
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let has_tunnels = !agent.tunnels.is_empty() || !agent.pending_tunnels.is_empty();
        if !has_tunnels {
            return self.render_no_tunnels(&agent.agent_id);
        }

        let online = agent.tunnels.iter().filter(|t| !t.is_disabled).count();
        let disabled = agent.tunnels.len() - online;
        // The disabled count is orange so a problem row scrolled out of view stays noticed.
        let mut summary = div()
            .flex()
            .items_center()
            .gap_1()
            .child(format!("{online} active"));
        // The disabled count is a link to the row, which may be scrolled out of view.
        if disabled > 0 {
            summary = summary.child("·").child(
                div()
                    .id("jump-to-disabled")
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .cursor_pointer()
                    .text_color(theme::warning().text)
                    .underline()
                    .child(format!("{disabled} disabled"))
                    .child(icon(Icon::ArrowDown, 12., theme::attention()))
                    .on_click(cx.listener(|view, _, _, cx| view.scroll_to_attention(cx))),
            );
        }
        if !agent.pending_tunnels.is_empty() {
            summary = summary
                .child("·")
                .child(format!("{} setting up", agent.pending_tunnels.len()));
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

        // Working addresses come first since sharing them is the main task; disabled tunnels
        // go last, and the header's count still flags them.
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut disabled_rows: Vec<AnyElement> = Vec::new();
        self.attention_row.set(None);
        let mut attention_sink = Some(self.attention_row.clone());
        for (ix, tunnel) in agent.tunnels.iter().enumerate() {
            if tunnel.is_disabled {
                disabled_rows.push(
                    tunnel_row(
                        ("tunnel", ix),
                        compact,
                        narrow,
                        TunnelRow {
                            address: tunnel.display_address.clone(),
                            has_address: true,
                            status: Some(("Disabled".into(), theme::warning())),
                            quiet_status: false,
                            detail: div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .min_w_0()
                                .text_color(theme::muted())
                                .child(icon(Icon::Alert, 13., theme::attention()))
                                // The reason says how to fix the tunnel, so it wraps rather
                                // than truncating at narrow widths.
                                .child(div().flex_1().min_w_0().child(disabled_explanation(
                                    tunnel.disabled_reason.as_deref(),
                                ))),
                            needs_attention: true,
                            position: attention_sink.take(),
                            // A disabled tunnel's address doesn't work, so where other rows copy
                            // it this one offers the fix, in orange so it reads as a remedy.
                            action: Some({
                                let url = tunnel_url(&tunnel.id);
                                row_button(
                                    ("fix-tunnel", ix),
                                    if narrow {
                                        "Fix tunnel"
                                    } else {
                                        "Fix on playit.gg"
                                    },
                                    ButtonStyle::Warning,
                                    Icon::External,
                                )
                                .on_click(move |_, _, cx| cx.open_url(&url))
                                .into_any_element()
                            }),
                            // The fix button already opens this tunnel's page.
                            manage_url: None,
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
                        narrow,
                        TunnelRow {
                            address: tunnel.display_address.clone(),
                            has_address: true,
                            status: Some((
                                if narrow { "Active" } else { "Tunnel active" }.into(),
                                theme::success(),
                            )),
                            quiet_status: true,
                            detail: forwards_to(&tunnel.destination),
                            needs_attention: false,
                            position: None,
                            action: Some(
                                row_button(
                                    ("copy-tunnel", ix),
                                    "Copy address",
                                    ButtonStyle::Secondary,
                                    Icon::Copy,
                                )
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.copy(address.clone(), "address", cx)
                                }))
                                .into_any_element(),
                            ),
                            manage_url: Some(tunnel_url(&tunnel.id)),
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
                    narrow,
                    TunnelRow {
                        address: pending.id.clone(),
                        has_address: false,
                        status: Some(("Setting up".into(), theme::info())),
                        quiet_status: false,
                        detail: div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .min_w_0()
                            .text_color(theme::muted())
                            .child(spinner(("pending-spinner", ix), 12., theme::progress()))
                            .child(div().flex_1().min_w_0().child(format!(
                                "{}. The address appears here when it's ready.",
                                sentence_case(&pending.status_msg)
                            ))),
                        needs_attention: false,
                        position: None,
                        // The detail line already says the address is coming, and without
                        // actions the explanation gets the row's full width.
                        action: None,
                        manage_url: None,
                    },
                )
                .into_any_element(),
            );
        }

        rows.extend(disabled_rows);

        let title = div()
            .flex()
            .flex_col()
            .gap_0p5()
            .min_w_0()
            .child(section_title("Tunnels", summary))
            .child(div().text_xs().text_color(theme::faint()).child(if narrow {
                "Your game server must be running for friends to join."
            } else {
                "Share an address with friends. An active tunnel doesn't start your game; your server must also be running."
            }));

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
                        .on_click({
                            let url = new_tunnel_url(&agent.agent_id);
                            move |_, _, cx| cx.open_url(&url)
                        }),
                    ),
            )
            .child(div().flex().flex_col().children(rows))
            .into_any_element()
    }

    /// The empty state fills the same full-width Tunnels panel the running view uses, so
    /// the screen keeps its structure once the first tunnel arrives.
    fn render_no_tunnels(&self, agent_id: &str) -> AnyElement {
        let new_tunnel = new_tunnel_url(agent_id);
        panel()
            .flex()
            .flex_col()
            .flex_none()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2p5()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .text_size(px(SECTION_TITLE_SIZE))
                            .font_weight(FontWeight::BOLD)
                            .child("Tunnels"),
                    ),
            )
            .child(
                // Stacked and centered like the other states' cards: icon, heading, text, action.
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .p_6()
                    .child(
                        div()
                            .size(px(48.))
                            .min_h(px(48.))
                            .flex_none()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme::brand().surface)
                            .border_1()
                            .border_color(theme::brand().border)
                            .child(icon(Icon::Globe, 24., theme::primary_text())),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::BOLD)
                                    .child("Add your first tunnel"),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .text_center()
                                    .text_color(theme::muted())
                                    .child("A tunnel gives your game server a public address your friends can join."),
                            )
                            .child(
                                large(button(
                                    "add-tunnel",
                                    "Add a tunnel on playit.gg",
                                    ButtonStyle::Primary,
                                    Some(Icon::External),
                                ))
                                .on_click(move |_, _, cx| cx.open_url(&new_tunnel)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::faint())
                                    .child("New tunnels show up here automatically."),
                            ),
                    ),
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
                    .px_4()
                    .pt_3()
                    .child(section_title("Traffic", "totals since playit started")),
            )
            .child(
                div()
                    .flex()
                    .child(stat_cell(
                        Icon::ArrowDown,
                        "Received by this computer",
                        format_bytes(stats.bytes_in),
                        None,
                        true,
                    ))
                    .child(stat_cell(
                        Icon::ArrowUp,
                        "Sent by this computer",
                        format_bytes(stats.bytes_out),
                        None,
                        true,
                    ))
                    .child(stat_cell(
                        Icon::Plug,
                        "Open connections now",
                        (stats.active_tcp + stats.active_udp).to_string(),
                        Some(format!(
                            "{} TCP, {} UDP",
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

        let plural = |count: usize, word: &str| {
            format!("{count} {word}{}", if count == 1 { "" } else { "s" })
        };
        let mut summary = div()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(theme::faint())
            .child(plural(visible.len(), "message"));
        // While the agent runs, logged problems are history: they're counted in muted text
        // and called "logged" so they don't read as a current fault.
        if healthy && (errors > 0 || warnings > 0) {
            let mut counts = Vec::new();
            if errors > 0 {
                counts.push(plural(errors, "error"));
            }
            if warnings > 0 {
                counts.push(plural(warnings, "warning"));
            }
            summary = summary.child(format!("· {} logged this session", counts.join(" and ")));
        }

        let title = div()
            .id("toggle-logs")
            .flex()
            .flex_1()
            .items_center()
            .gap_3()
            .h_full()
            .px_4()
            .cursor_pointer()
            .hover(|style| style.bg(theme::interactive()))
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
                        theme::faint(),
                    ))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::BOLD)
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
                .pr_4()
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
                .bg(theme::bg())
                .font_family(mono_font())
                .text_size(px(12.5))
                .child(list)
                .when(visible.len() > LOG_MAX_ROWS, |area| {
                    // A fade over the top edge hides the partly scrolled-off line.
                    area.child(div().absolute().top_0().left_0().right_0().h(px(16.)).bg(
                        linear_gradient(
                            180.,
                            linear_color_stop(theme::bg(), 0.),
                            linear_color_stop(theme::bg().opacity(0.), 1.),
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
                card.tone = theme::info();
                card.title = "Connecting to playit".into();
                card.body =
                    vec!["Looking for the playit background service on this computer.".into()];
                card.waiting = Some("Connecting…".into());
            }
            Screen::StartingService => {
                card.badge = Badge::Spinner;
                card.tone = theme::info();
                card.title = "Starting the playit service".into();
                card.body = vec![if START_NEEDS_PASSWORD {
                    "If your computer asks for your password, enter it to let playit start its background service.".into()
                } else {
                    "The playit background service on this computer wasn't running, so playit is starting it.".into()
                }];
                card.waiting = Some("Starting…".into());
            }
            Screen::Unreachable { error, start_error } => {
                card.badge = Badge::Icon(Icon::Power);
                card.tone = theme::danger();
                let guidance =
                    unreachable_guidance(self.state.session_mode, error, start_error.is_some());
                card.title = guidance.title.into();
                card.body = guidance.body;
                card.command = guidance.command;
                let mut details = format!("Address: {}\n{}", self.state.socket_path, error);
                if let Some(start_error) = start_error {
                    details.push_str(&format!("\nStarting the service failed: {start_error}"));
                }
                card.details = Some(details);
                card.waiting = Some("Reconnecting automatically every few seconds…".into());
                if guidance.offer_start {
                    card.actions.push(
                        large(button(
                            "start-service",
                            "Start service",
                            ButtonStyle::Primary,
                            Some(Icon::Power),
                        ))
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.send(UiCommand::StartService);
                            cx.notify();
                        }))
                        .into_any_element(),
                    );
                }
                card.actions.push(
                    large(button(
                        "reconnect",
                        "Retry now",
                        if guidance.offer_start {
                            ButtonStyle::Secondary
                        } else {
                            ButtonStyle::Primary
                        },
                        Some(Icon::Refresh),
                    ))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.send(UiCommand::Reconnect);
                        cx.notify();
                    }))
                    .into_any_element(),
                );
                card.actions.push(link_button(
                    "unreachable-help",
                    "Troubleshooting guide",
                    SUPPORT_URL,
                    ButtonStyle::Secondary,
                ));
            }
            Screen::Starting => {
                card.badge = Badge::Spinner;
                card.tone = theme::info();
                card.title = "Starting playit".into();
                card.body = vec!["This usually takes a few seconds.".into()];
                if self.state.lifecycle_since.elapsed() >= SLOW_START {
                    card.body.push(
                        "This is taking longer than usual. The activity log below shows what is happening.".into(),
                    );
                }
            }
            Screen::Stopping => {
                card.badge = Badge::Icon(Icon::Clock);
                card.tone = theme::neutral();
                card.title = "Stopping playit".into();
                card.body = vec!["Your tunnels are going offline.".into()];
                card.waiting = Some("Stopping…".into());
            }
            Screen::Error { message } => {
                let hint = error_hint(message);
                // playit keeps retrying, so this reads as a problem to watch, not a dead end.
                card.badge = Badge::Icon(Icon::Alert);
                card.tone = theme::warning();
                card.title = hint.title.into();
                card.body = vec![hint.next_step.to_string()];
                card.details = Some(message.to_string());
                card.waiting = Some("Retrying automatically…".into());
                card.action_note = hint.action_note.map(str::to_string);
                card.actions.push(link_button(
                    "get-help",
                    "Troubleshooting guide",
                    SUPPORT_URL,
                    ButtonStyle::Primary,
                ));
            }
            Screen::InvalidSecret { message } => {
                card.badge = Badge::Icon(Icon::Link);
                card.tone = theme::warning();
                card.title = "Link this computer again".into();
                card.body = vec![
                    "This computer's link to your playit account stopped working, so your tunnels are offline. It may have been removed from your account.".into(),
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
                card.tone = theme::warning();
                card.title = "Computer limit reached".into();
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
                card.tone = theme::brand();
                card.title = "Connect playit to your account".into();
                card.body = vec![
                    "Link this computer to your playit account to start sharing your servers."
                        .into(),
                ];
                match &setup.step {
                    SetupStep::Rejected | SetupStep::Failed(_) => {
                        card.badge = Badge::Icon(Icon::XCircle);
                        card.tone = theme::danger();
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

        // Every state's card starts at the same height so its heading stays put as the state
        // changes.
        let mut group = div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(CARD_WIDTH))
            .max_w_full()
            .child(card.render(self.details_expanded, cx));
        let has_problems = self
            .state
            .logs
            .iter()
            .any(|entry| matches!(entry.level, LogLevel::Warn | LogLevel::Error));
        if !self.state.logs.is_empty() && (has_problems || !matches!(screen, Screen::Setup(_))) {
            group = group.child(self.render_logs(false, cx));
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .child(div().flex_none().h(px(CARD_TOP_OFFSET)))
            .child(group)
    }

    fn render_footer(
        &self,
        screen: &Screen,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let left: AnyElement = if matches!(screen, Screen::Running(_)) {
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child("Closing this window keeps playit and your tunnels running."),
                )
                .into_any_element()
        } else {
            // The recovery screens have nothing to say about closing the window, so the
            // footer offers help instead of sitting empty.
            help_link().into_any_element()
        };

        let mut right = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .text_color(theme::faint());
        // The other screens put help on the left; here the left holds the reassurance.
        if matches!(screen, Screen::Running(_)) {
            right = right.child(help_link());
        }
        // Narrow windows leave the computer ID out; it's for support conversations, which
        // happen with the window at a comfortable size.
        if let Screen::Running(agent) = screen
            && !agent.agent_id.is_empty()
            && !narrow
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
                    .text_color(theme::placeholder())
                    .hover(|style| style.text_color(theme::muted()))
                    .child(format!("Computer ID {short_id}"))
                    .child(icon(Icon::Copy, 12., theme::placeholder()))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.copy(agent_id.clone(), "agent ID", cx)
                    })),
            );
        }
        let version = match screen {
            Screen::Running(agent) if !agent.version.is_empty() => agent.version.clone(),
            _ => env!("CARGO_PKG_VERSION").to_string(),
        };
        right = right.child(
            div()
                .text_color(theme::placeholder())
                .child(format!("v{version}")),
        );

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .px_5()
            .h(px(36.))
            .border_t_1()
            .border_color(theme::border_muted())
            .bg(theme::bg())
            .text_xs()
            .text_color(theme::faint())
            .child(div().flex().min_w_0().child(left))
            .child(right)
    }
}

impl Render for PlayitWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let compact = viewport.height < px(COMPACT_HEIGHT);
        let narrow = viewport.width < px(NARROW_WIDTH);
        let decorations = window.window_decorations();
        let chrome = match decorations {
            Decorations::Server => None,
            Decorations::Client { .. } => {
                window.set_client_inset(px(RESIZE_INSET));
                Some(WindowChrome {
                    maximized: window.is_maximized(),
                    controls: window.window_controls(),
                })
            }
        };
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
            .font_family(UI_FONT)
            .text_color(theme::text())
            .text_sm()
            // The website's body weight is Inter Medium.
            .font_weight(FontWeight::MEDIUM)
            .child(self.render_header(&screen, compact, narrow, chrome));

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
                    .bg(theme::danger().surface)
                    .border_l(px(4.))
                    .border_color(theme::failure_strong())
                    .text_color(theme::text())
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

        // The content scrolls only when the window is too short for it. A scrollbar and a
        // fade at the bottom edge show that more is below.
        let max_scroll = self.content_scroll.max_offset().height;
        let scrolled = -self.content_scroll.offset().y;
        let more_below = max_scroll > px(1.) && scrolled < max_scroll - px(1.);
        let viewport_height = self.content_scroll.bounds().size.height;
        let scrollbar = (max_scroll > px(1.) && viewport_height > px(0.)).then(|| {
            let track = viewport_height - px(8.);
            let thumb = (track * (viewport_height / (viewport_height + max_scroll))).max(px(32.));
            let top = px(4.) + (track - thumb) * (scrolled / max_scroll).clamp(0., 1.);
            div()
                .absolute()
                .top(top)
                .right(px(3.))
                .w(px(6.))
                .h(thumb)
                .rounded_full()
                .bg(theme::border_strong())
        });
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
                    .children(scrollbar)
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
            .child(self.render_footer(&screen, narrow, cx));

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
                    .rounded(px(RADIUS_SM))
                    .bg(theme::success().surface)
                    .border_1()
                    .border_color(theme::success().border)
                    .text_color(theme::text())
                    .shadow_lg()
                    .child(icon(Icon::Check, 14., theme::online()))
                    .child(toast.clone()),
            );
        }
        match decorations {
            Decorations::Server => root.into_any_element(),
            Decorations::Client { tiling } => resize_frame(tiling, root).into_any_element(),
        }
    }
}

/// Window state the custom title bar needs when the OS leaves decorations to the app.
#[derive(Clone, Copy)]
struct WindowChrome {
    maximized: bool,
    controls: WindowControls,
}

/// Width of the invisible band around a client-decorated window that resizes it.
const RESIZE_INSET: f32 = 5.;

fn window_controls(chrome: WindowChrome) -> impl IntoElement {
    let control = |id: &'static str, kind: Icon, close: bool| {
        div()
            .id(id)
            .group(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(32.))
            .rounded(px(RADIUS))
            .cursor_pointer()
            .hover(move |style| {
                if close {
                    style.bg(theme::failure_strong())
                } else {
                    style.bg(theme::interactive())
                }
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(icon(kind, 15., theme::muted()).when(close, |x| {
                x.group_hover(id, |style| style.text_color(theme::on_primary()))
            }))
    };
    div()
        .flex()
        .items_center()
        .gap_1()
        .ml_1()
        .pl_2()
        .border_l_1()
        .border_color(theme::border())
        .when(chrome.controls.minimize, |row| {
            row.child(
                control("window-minimize", Icon::Minus, false)
                    .on_click(|_, window, _| window.minimize_window()),
            )
        })
        .when(chrome.controls.maximize, |row| {
            row.child(
                control(
                    "window-maximize",
                    if chrome.maximized {
                        Icon::Restore
                    } else {
                        Icon::Maximize
                    },
                    false,
                )
                .on_click(|_, window, _| window.zoom_window()),
            )
        })
        .child(
            control("window-close", Icon::X, true).on_click(|_, window, _| window.remove_window()),
        )
}

/// Wraps client-decorated content in a thin band that resizes the window from its edges.
fn resize_frame(tiling: Tiling, content: impl IntoElement) -> impl IntoElement {
    let inset = px(RESIZE_INSET);
    div()
        .id("resize-frame")
        .size_full()
        .bg(theme::border())
        .when(!tiling.top, |frame| frame.pt(inset))
        .when(!tiling.bottom, |frame| frame.pb(inset))
        .when(!tiling.left, |frame| frame.pl(inset))
        .when(!tiling.right, |frame| frame.pr(inset))
        .child(
            canvas(
                |_bounds, window, _cx| {
                    window.insert_hitbox(
                        Bounds::new(point(px(0.), px(0.)), window.viewport_size()),
                        HitboxBehavior::Normal,
                    )
                },
                move |_bounds, hitbox, window, _cx| {
                    let size = window.viewport_size();
                    if let Some(edge) = resize_edge(window.mouse_position(), inset, size) {
                        window.set_cursor_style(resize_cursor(edge), &hitbox);
                    }
                },
            )
            .size_full()
            .absolute(),
        )
        .on_mouse_move(|_, window, _| window.refresh())
        .on_mouse_down(MouseButton::Left, move |event, window, _| {
            if let Some(edge) = resize_edge(event.position, inset, window.viewport_size()) {
                window.start_window_resize(edge);
            }
        })
        .child(
            div()
                .size_full()
                .cursor(CursorStyle::Arrow)
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .child(content),
        )
}

fn resize_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

fn resize_edge(pos: Point<Pixels>, inset: Pixels, size: Size<Pixels>) -> Option<ResizeEdge> {
    let top = pos.y < inset;
    let bottom = pos.y > size.height - inset;
    let left = pos.x < inset;
    let right = pos.x > size.width - inset;
    Some(match (top, bottom, left, right) {
        (true, _, true, _) => ResizeEdge::TopLeft,
        (true, _, _, true) => ResizeEdge::TopRight,
        (_, true, true, _) => ResizeEdge::BottomLeft,
        (_, true, _, true) => ResizeEdge::BottomRight,
        (true, ..) => ResizeEdge::Top,
        (_, true, ..) => ResizeEdge::Bottom,
        (_, _, true, _) => ResizeEdge::Left,
        (_, _, _, true) => ResizeEdge::Right,
        _ => return None,
    })
}

enum NoticeAction {
    None,
    Link(String),
    GuestLogin(Option<String>),
}

fn notice_tone(notice: &NoticeState) -> Tone {
    match notice.priority.to_ascii_lowercase().as_str() {
        "high" | "critical" | "error" => theme::danger(),
        "low" | "info" => theme::info(),
        _ => theme::warning(),
    }
}

/// The account reads in neutral tones so green stays reserved for connection state.
fn render_account(status: &AccountStatus) -> AnyElement {
    let row = div().flex().items_center().gap_1p5();
    match status {
        AccountStatus::Verified => row
            .child(icon(Icon::User, 14., theme::muted()))
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
    title: &'static str,
    next_step: &'static str,
    /// Something to try before the troubleshooting guide, when there is one.
    action_note: Option<&'static str>,
}

/// Plain-language headings and next steps for agent errors with a recognizable cause.
fn error_hint(message: &str) -> ErrorHint {
    let message = message.to_ascii_lowercase();
    if message.contains("address already in use") || message.contains("address in use") {
        ErrorHint {
            title: "Network port already in use",
            next_step: "Another program is using a network port playit needs, so your tunnels are offline.",
            action_note: Some(
                "If another copy of playit is running, close it. The guide covers other causes.",
            ),
        }
    } else if message.contains("permission denied") || message.contains("access is denied") {
        ErrorHint {
            title: "The playit service is missing a permission",
            next_step: "Restarting the playit service usually fixes this.",
            action_note: None,
        }
    } else if message.contains("timed out")
        || message.contains("dns")
        || message.contains("resolve")
    {
        ErrorHint {
            title: "Can't reach the playit network",
            next_step: "Check your internet connection and firewall.",
            action_note: None,
        }
    } else {
        ErrorHint {
            title: "Something interrupted playit",
            next_step: "Your tunnels are offline until it recovers.",
            action_note: None,
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
    tone: Tone,
    title: SharedString,
    body: Vec<String>,
    steps: Option<[StepState; 2]>,
    link: Option<String>,
    command: Option<String>,
    details: Option<String>,
    /// What playit is doing on its own, shown after the actions.
    waiting: Option<SharedString>,
    /// A sentence introducing the action buttons, shown right above them.
    action_note: Option<String>,
    actions: Vec<AnyElement>,
}

impl Default for MessageCard {
    fn default() -> Self {
        Self {
            badge: Badge::Icon(Icon::Info),
            tone: theme::neutral(),
            title: SharedString::default(),
            body: Vec::new(),
            steps: None,
            link: None,
            command: None,
            details: None,
            waiting: None,
            action_note: None,
            actions: Vec::new(),
        }
    }
}

impl MessageCard {
    fn render(self, details_expanded: bool, cx: &mut Context<PlayitWindow>) -> impl IntoElement {
        let badge: AnyElement = match self.badge {
            Badge::Icon(kind) => icon(kind, 24., self.tone.text).into_any_element(),
            Badge::Spinner => spinner("badge-spinner", 24., self.tone.text).into_any_element(),
        };
        // Like the website's setup wizard card: a stronger border and rounder corners.
        let mut card = panel()
            .border_color(theme::border_strong())
            .rounded(px(RADIUS_SM))
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
                    .bg(self.tone.surface)
                    .border_1()
                    .border_color(self.tone.border)
                    .child(badge),
            )
            .child(
                div()
                    .w(px(CARD_WIDTH - CARD_PADDING_AND_BORDER))
                    .max_w_full()
                    .text_size(px(22.))
                    .font_weight(FontWeight::BOLD)
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
                                .font_family(mono_font())
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

        // The explanation leads straight into its actions; what playit does on its own
        // follows them.
        let waiting = self.waiting.map(|waiting| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(13.))
                .text_color(theme::faint())
                .child(spinner("waiting-spinner", 14., theme::progress()))
                .child(waiting)
        });
        if let Some(note) = self.action_note {
            card = card.child(div().text_color(theme::muted()).child(note));
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
                                    .font_family(mono_font())
                                    .text_xs()
                                    .text_color(theme::link())
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

        if let Some(line) = waiting {
            card = card.child(line);
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
                                    .text_size(px(13.))
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
                                .font_family(mono_font())
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
    const LABELS: [&str; 2] = [
        "Open the setup page",
        "Sign in or create a free account, then approve this computer",
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
                    // The website's setup wizard: cyan for the current step, green once done, dim until reached.
                    let (marker_bg, marker_border, marker_fg, text) = match state {
                        StepState::Done => (
                            theme::success().surface,
                            theme::online_strong(),
                            theme::online(),
                            theme::muted(),
                        ),
                        StepState::Active => (
                            theme::info().border,
                            theme::link(),
                            theme::link_hover(),
                            theme::text(),
                        ),
                        StepState::Todo => (
                            theme::header_band(),
                            theme::border_strong(),
                            theme::faint(),
                            theme::faint(),
                        ),
                    };
                    let marker = div()
                        .size(px(24.))
                        .min_h(px(24.))
                        .flex_none()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_2()
                        .border_color(marker_border)
                        .bg(marker_bg)
                        .text_color(marker_fg)
                        .text_xs()
                        .font_weight(FontWeight::BOLD);
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
    /// Show a Start service button, for services that only start with a password.
    offer_start: bool,
}

fn unreachable_guidance(mode: SessionMode, error: &str, start_failed: bool) -> UnreachableGuidance {
    // The service answered but speaks another IPC version, so restarting it won't help.
    if error.contains("IPC protocol mismatch") {
        return UnreachableGuidance {
            title: "playit needs an update",
            body: vec![
                "This window and the playit background service are from different versions of playit, so they can't work together. Install the latest playit to update both.".into(),
            ],
            command: None,
            offer_start: false,
        };
    }
    if mode == SessionMode::ExternalDaemon {
        return UnreachableGuidance {
            title: "Can't reach the playit service",
            body: vec![
                "The background part of playit that runs your tunnels isn't responding, so this window can't show their status. If this keeps happening, restart your computer.".into(),
            ],
            command: None,
            offer_start: false,
        };
    }
    if cfg!(target_os = "linux") && error.contains("Permission denied") {
        return UnreachableGuidance {
            title: "Your user needs permission to use playit",
            body: vec![
                "The playit service is running, but your user isn't allowed to talk to it yet. Add yourself to the playit group, then log out and back in.".into(),
            ],
            command: Some("sudo usermod -aG playit $USER".into()),
            offer_start: false,
        };
    }
    // `playit start` on Linux needs to know the init system, so name systemd directly.
    let command = if cfg!(target_os = "linux") {
        "sudo systemctl start playit"
    } else {
        "playit start"
    };
    let mut body = vec![
        "Your tunnels are offline until the playit background service on this computer starts again.".into(),
    ];
    if start_failed {
        body.push("playit couldn't start it. The details below say why.".into());
    }
    body.push(if START_NEEDS_PASSWORD {
        "Press Start service and enter your password when asked, or run this in a terminal:".into()
    } else {
        "Start it from a terminal. This window reconnects automatically:".into()
    });
    UnreachableGuidance {
        title: "The playit service isn't running",
        body,
        command: Some(command.into()),
        offer_start: START_NEEDS_PASSWORD,
    }
}

struct TunnelRow {
    address: String,
    /// Pending tunnels show their name, which isn't something to share yet.
    has_address: bool,
    /// The row's state in words, so status never depends on color alone.
    status: Option<(SharedString, Tone)>,
    /// The healthy state reads as a dot and plain text so the exceptions' badges stand out.
    quiet_status: bool,
    detail: Div,
    /// Marks a row that needs the user with an orange edge.
    needs_attention: bool,
    /// Receives the row's painted bounds, for the first row that needs attention.
    position: Option<Rc<Cell<Option<Bounds<Pixels>>>>>,
    action: Option<AnyElement>,
    /// The tunnel's page on playit.gg, linked from a quiet "Manage" action.
    manage_url: Option<String>,
}

/// One tunnel: the address with its status label, a detail line, and an action in a
/// fixed-width column so every row's button lines up.
fn tunnel_row(
    id: impl Into<ElementId>,
    compact: bool,
    narrow: bool,
    row: TunnelRow,
) -> impl IntoElement {
    let address_line = div()
        .flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(15.))
                .when(row.has_address, |text| text.font_weight(FontWeight::BOLD))
                // A disabled tunnel's address doesn't work, so it's slightly dimmed, but stays
                // readable so the tunnel is recognizable.
                .text_color(if row.needs_attention {
                    theme::muted()
                } else {
                    theme::text()
                })
                .child(row.address),
        )
        .children(row.status.map(|(label, tone)| {
            if row.quiet_status {
                quiet_status_label(label, tone.strong).into_any_element()
            } else {
                chip(label, tone).into_any_element()
            }
        }));
    let detail = div().flex().min_w_0().text_size(px(14.)).child(row.detail);
    // Rows with any action keep both slots so the buttons stay in columns. Narrow windows
    // shorten the Copy button so the address keeps its width and Manage its label.
    let has_actions = row.action.is_some() || row.manage_url.is_some();
    let needs_attention = row.needs_attention;
    let actions = div()
        .flex()
        .flex_none()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_none()
                .w(px(if narrow {
                    NARROW_ROW_ACTION_WIDTH
                } else {
                    ROW_ACTION_WIDTH
                }))
                .children(row.action),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .w(px(MANAGE_LINK_WIDTH))
                .children(row.manage_url.map(|url| {
                    button(
                        SharedString::from(format!("manage-{url}")),
                        "Manage",
                        ButtonStyle::Ghost,
                        Some(Icon::External),
                    )
                    .w_full()
                    .py_1p5()
                    .on_click(move |_, _, cx| cx.open_url(&url))
                })),
        );

    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .when(compact, |el| el.py_1p5())
        .when(!compact, |el| el.py_2())
        .border_b_1()
        .border_color(theme::border_muted())
        .hover(move |style| {
            style.bg(if needs_attention {
                theme::warning().surface
            } else {
                theme::interactive()
            })
        })
        .when_some(row.position, |row, sink| {
            row.relative().child(
                canvas(move |bounds, _, _| sink.set(Some(bounds)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
        })
        // A strip rather than a left border, since a border would recolor the row divider.
        .when(row.needs_attention, |row| {
            row.relative().bg(theme::warning().subtle).child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(6.))
                    .bottom(px(6.))
                    .w(px(4.))
                    .rounded_r_sm()
                    .bg(theme::attention_strong()),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(address_line)
                .child(detail),
        )
        .when(has_actions, |row| row.child(actions))
}

/// The Copy column in narrow windows, with tighter room around the button's label.
const NARROW_ROW_ACTION_WIDTH: f32 = 120.;

/// Width of the "Manage" link column on tunnel rows.
const MANAGE_LINK_WIDTH: f32 = 88.;

fn log_row(entry: &LogEntry, today: chrono::NaiveDate, show_target: bool) -> Div {
    let (level_color, level_fill, message_color) = match entry.level {
        LogLevel::Error => (
            theme::danger().text,
            Some(theme::danger().subtle),
            theme::text(),
        ),
        LogLevel::Warn => (
            theme::warning().text,
            Some(theme::warning().subtle),
            theme::text(),
        ),
        LogLevel::Info => (theme::faint(), None, theme::muted()),
        LogLevel::Debug | LogLevel::Trace => (theme::faint(), None, theme::faint()),
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
                    .rounded(px(4.))
                    .when_some(level_fill, |chip, fill| chip.bg(fill))
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

/// Space above the card on every non-running screen.
const CARD_TOP_OFFSET: f32 = 112.;

/// Explains why a tunnel is disabled and, for the reasons we recognize, how to turn it back on.
fn disabled_explanation(reason: Option<&str>) -> String {
    let reason = disabled_reason(reason);
    if reason.to_ascii_lowercase().contains("tunnel limit") {
        format!("{reason}. Remove a tunnel or upgrade your plan to turn it back on.")
    } else {
        format!("{reason}.")
    }
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
        (false, Some(port)) => format!("{host}, port {port}"),
        (false, None) => destination.to_string(),
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
        .rounded(px(RADIUS))
        .overflow_hidden()
}

fn inset() -> Div {
    div()
        .px_3()
        .py_2()
        .rounded(px(RADIUS))
        .bg(theme::inset())
        .border_1()
        .border_color(theme::border())
}

fn dot(color: Hsla) -> Div {
    div().flex_none().size(px(8.)).rounded_full().bg(color)
}

fn pill(label: &'static str, tone: Tone) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .px_2p5()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(tone.border)
        .bg(tone.subtle)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(tone.text)
        .child(dot(tone.strong))
        .child(label)
}

fn quiet_status_label(label: impl Into<SharedString>, color: Hsla) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .text_xs()
        .text_color(theme::muted())
        .child(dot(color))
        .child(label.into())
}

fn chip(label: impl Into<SharedString>, tone: Tone) -> impl IntoElement {
    div()
        .flex_none()
        .px_1p5()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(tone.border)
        .bg(tone.subtle)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(tone.text)
        .child(label.into())
}

/// The footer's link to playit's help pages, the same on every screen.
fn help_link() -> impl IntoElement {
    div()
        .id("footer-help")
        .flex()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_color(theme::faint())
        .hover(|style| style.text_color(theme::text()))
        .child(icon(Icon::Info, 12., theme::faint()))
        .child("Help")
        .on_click(|_, _, cx| cx.open_url(SUPPORT_URL))
}

/// A section heading with a caption beside it. Both share one line box so the caption's
/// text sits level with the heading's instead of hanging below it.
fn section_title(title: &'static str, caption: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .h(px(22.))
        .child(
            div()
                .flex_none()
                .line_height(px(22.))
                .text_size(px(SECTION_TITLE_SIZE))
                .font_weight(FontWeight::BOLD)
                .child(title),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .line_height(px(22.))
                .pt(px(1.))
                .text_size(px(13.))
                .text_color(theme::faint())
                .child(caption),
        )
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
    detail: Option<String>,
    divider: bool,
) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .px_4()
        .pt_1p5()
        .pb_2p5()
        .when(divider, |cell| {
            cell.border_r_1().border_color(theme::border_muted())
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(13.))
                .text_color(theme::faint())
                .child(icon(icon_kind, 14., theme::faint()))
                .child(div().truncate().child(label)),
        )
        .child(
            div()
                .flex()
                .items_end()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex_none()
                        .text_xl()
                        .font_weight(FontWeight::BOLD)
                        .child(value),
                )
                // Bottom-aligned with a small lift so the small text's baseline meets the
                // value's.
                .children(detail.map(|detail| {
                    div()
                        .min_w_0()
                        .truncate()
                        .pb(px(3.))
                        .text_xs()
                        .text_color(theme::faint())
                        .child(detail)
                })),
        )
}

fn toggle(id: &'static str, label: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    let marker = div()
        .size(px(14.))
        .flex_none()
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .border_1();
    let marker = if on {
        marker
            .border_color(theme::primary_border())
            .bg(theme::primary())
            .child(icon(Icon::Check, 10., theme::on_primary()))
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
    /// A remedy for something that needs attention, such as a disabled tunnel.
    Warning,
}

fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    style: ButtonStyle,
    leading_icon: Option<Icon>,
) -> Stateful<Div> {
    let (bg, hover_bg, border, text) = match style {
        ButtonStyle::Primary => (
            theme::primary(),
            theme::primary_hover(),
            theme::primary_border(),
            theme::on_primary(),
        ),
        ButtonStyle::Secondary => (
            theme::panel(),
            theme::secondary_hover_bg(),
            theme::secondary(),
            theme::text(),
        ),
        ButtonStyle::Ghost => (
            gpui::transparent_black(),
            theme::interactive(),
            gpui::transparent_black(),
            theme::muted(),
        ),
        ButtonStyle::Warning => (
            theme::warning().subtle,
            theme::warning().surface,
            theme::primary_border(),
            theme::warning().text,
        ),
    };
    let ghost = matches!(style, ButtonStyle::Ghost);
    let label = label.into();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .px_2p5()
        .py_1()
        .rounded(px(if ghost { RADIUS_SM } else { RADIUS }))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(text)
        .cursor_pointer()
        .hover(move |style| {
            let style = style.bg(hover_bg);
            if ghost {
                style.text_color(theme::text())
            } else {
                style
            }
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .children(leading_icon.map(|kind| icon(kind, 13., text)))
        .when(!label.is_empty(), |button| button.child(label))
}

/// A full-width, left-aligned row action, so every row's icon and label start at the same x.
fn row_button(
    id: impl Into<ElementId>,
    label: &'static str,
    style: ButtonStyle,
    kind: Icon,
) -> Stateful<Div> {
    button(id, label, style, Some(kind))
        .w_full()
        .justify_center()
        .px_3()
        .py_1p5()
}

/// A taller button for the main action of a screen.
fn large(button: Stateful<Div>) -> Stateful<Div> {
    button
        .px_4()
        .py_2()
        .text_sm()
        .rounded(px(RADIUS))
        .min_h(px(36.))
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
    fn version_mismatch_asks_for_an_update() {
        let guidance = super::unreachable_guidance(
            crate::state::SessionMode::Service,
            "IPC protocol mismatch: expected version 3, got 2",
            false,
        );
        assert_eq!(guidance.title, "playit needs an update");
        assert!(guidance.command.is_none());
    }

    #[test]
    fn start_button_only_where_starting_asks_for_a_password() {
        let stopped = super::unreachable_guidance(
            crate::state::SessionMode::Service,
            "No such file or directory (os error 2)",
            false,
        );
        assert_eq!(stopped.offer_start, crate::backend::START_NEEDS_PASSWORD);
        assert!(stopped.command.is_some());

        let custom_socket = super::unreachable_guidance(
            crate::state::SessionMode::ExternalDaemon,
            "No such file or directory (os error 2)",
            false,
        );
        assert!(!custom_socket.offer_start);
    }

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
            "192.168.1.20, port 19132"
        );
    }
}
