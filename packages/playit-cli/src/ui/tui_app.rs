use std::collections::VecDeque;
use std::io::{self, Stdout, stdout};
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, enable_raw_mode},
};
use playit_ipc::model::{
    AccountStatus as ServiceAccountStatus, AgentLifecycle, AgentState as ServiceAgentState,
    ConnectionStats as ServiceConnectionStats, LogEntry, LogLevel, ServiceStatus,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use super::widgets::{render_header, render_help_bar, render_stats_bar};
use crate::CliError;
use crate::signal_handle::get_signal_handle;

const SERVICE_LOG_CAPACITY: usize = 500;
const ACCOUNT_AGENTS_URL: &str = "https://playit.gg/account/agents";
const ACCOUNT_UPGRADE_URL: &str = "https://playit.gg/account/upgrade";

/// Data about the running agent
#[derive(Clone, Default)]
pub struct AgentData {
    pub version: String,
    pub tunnels: Vec<TunnelInfo>,
    pub pending_tunnels: Vec<PendingTunnelInfo>,
    pub notices: Vec<NoticeInfo>,
    pub account_status: AccountStatusInfo,
    pub agent_id: String,
    pub login_link: Option<String>,
    /// Start time of the agent/service in milliseconds since epoch
    pub start_time: u64,
}

#[derive(Clone, Debug)]
pub struct TunnelInfo {
    pub display_address: String,
    pub destination: String,
    pub is_disabled: bool,
    pub disabled_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PendingTunnelInfo {
    pub id: String,
    pub status_msg: String,
}

#[derive(Clone, Debug)]
pub struct NoticeInfo {
    pub priority: String,
    pub message: String,
    pub resolve_link: Option<String>,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub enum AccountStatusInfo {
    #[default]
    Unknown,
    Guest,
    EmailNotVerified,
    Verified,
}

/// Connection statistics
#[derive(Clone, Default)]
pub struct ConnectionStats {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_tcp: u32,
    pub active_udp: u32,
}

/// UI mode for TuiApp
#[derive(Clone, Debug, PartialEq)]
pub enum TuiMode {
    Message { message: String },
    Running,
}

/// Main TUI application state
pub struct TuiApp {
    service_logs: VecDeque<LogEntry>,
    agent_data: AgentData,
    stats: ConnectionStats,

    // UI state
    mode: TuiMode,
    should_quit: bool,
    quit_confirm: bool,

    // Terminal
    terminal: Option<Terminal<CrosstermBackend<Stdout>>>,
}

impl TuiApp {
    pub fn new() -> Self {
        Self {
            service_logs: VecDeque::with_capacity(SERVICE_LOG_CAPACITY),
            agent_data: AgentData::default(),
            stats: ConnectionStats::default(),
            mode: TuiMode::Message {
                message: "Initializing...".to_string(),
            },
            should_quit: false,
            quit_confirm: false,
            terminal: None,
        }
    }

    pub fn set_message<T: Into<String>>(&mut self, message: T) {
        self.mode = TuiMode::Message {
            message: message.into(),
        };
    }

    pub fn set_agent_data(&mut self, data: AgentData) {
        self.agent_data = data;
        self.mode = TuiMode::Running;
    }

    pub fn set_stats(&mut self, stats: ConnectionStats) {
        self.stats = stats;
    }

    pub fn push_service_log(&mut self, entry: LogEntry) {
        if self.service_logs.len() >= SERVICE_LOG_CAPACITY {
            self.service_logs.pop_front();
        }
        self.service_logs.push_back(entry);
    }

    pub fn apply_lifecycle(&mut self, lifecycle: AgentLifecycle) {
        match lifecycle {
            AgentLifecycle::Running(state) => self.set_agent_data(state.into()),
            AgentLifecycle::WaitingForSecret => {
                self.set_message("The playit service is waiting for setup to finish.");
            }
            AgentLifecycle::HasInvalidSecret(error) => self.set_message(format!(
                "The playit service has an invalid secret: {}",
                error.message
            )),
            AgentLifecycle::DisabledOverLimit(_) => self.set_message(format!(
                "{}\n{}",
                agent_over_limit_title(),
                agent_over_limit_guidance()
            )),
            AgentLifecycle::Starting => {
                self.set_message("The playit service is starting...");
            }
            AgentLifecycle::Stopping => {
                self.set_message("The playit service is stopping...");
            }
            AgentLifecycle::Error(error) => {
                self.set_message(format!(
                    "The playit service reported an error: {}",
                    error.message
                ));
            }
        }
    }

    pub fn apply_status(&mut self, status: ServiceStatus) {
        if let Some(message) = status_message(&status) {
            self.set_message(message);
        }
    }

    /// Initialize the terminal for TUI mode
    fn init_terminal(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        self.terminal = Some(Terminal::new(backend)?);
        Ok(())
    }

    /// Restore the terminal to normal mode
    fn restore_terminal(&mut self) -> io::Result<()> {
        if let Some(mut terminal) = self.terminal.take() {
            crossterm::terminal::disable_raw_mode()?;
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
            terminal.show_cursor()?;
        }
        Ok(())
    }

    pub fn init(&mut self) -> Result<(), CliError> {
        if self.terminal.is_none() {
            self.init_terminal().map_err(CliError::RenderError)?;
        }
        Ok(())
    }

    pub fn shutdown(&mut self) -> Result<(), CliError> {
        self.restore_terminal().map_err(CliError::RenderError)
    }

    /// Run one iteration of the TUI (draw + handle events)
    /// Returns Ok(true) if should continue, Ok(false) if should quit
    pub fn tick(&mut self) -> Result<bool, CliError> {
        if self.terminal.is_none() {
            self.init()?;
        }

        self.draw().map_err(CliError::RenderError)?;

        if event::poll(Duration::from_millis(50)).map_err(CliError::RenderError)? {
            if let Event::Key(key) = event::read().map_err(CliError::RenderError)? {
                self.handle_key_event(key);
            }
        }

        let signal = get_signal_handle();
        if signal.is_confirming_close() && !self.quit_confirm {
            self.quit_confirm = true;
        }

        Ok(!self.should_quit)
    }

    fn handle_key_event(&mut self, key: KeyEvent) {
        if self.quit_confirm {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.should_quit = true;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.quit_confirm = false;
                    get_signal_handle().decline_close();
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => {
                self.quit_confirm = true;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit_confirm = true;
            }
            _ => {}
        }
    }

    fn draw(&mut self) -> io::Result<()> {
        let terminal = self.terminal.as_mut().unwrap();

        let mode = self.mode.clone();
        let agent_data = self.agent_data.clone();
        let stats = self.stats.clone();
        let start_time = agent_data.start_time;
        let quit_confirm = self.quit_confirm;
        let log_entries: Vec<_> = self.service_logs.iter().cloned().collect();

        terminal.draw(|frame| {
            let area = frame.area();

            match &mode {
                TuiMode::Message { message } => {
                    Self::render_message_screen(frame, area, message, quit_confirm);
                    return;
                }
                TuiMode::Running => {}
            }

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(3),
                    Constraint::Length(10),
                    Constraint::Length(1),
                ])
                .split(area);

            render_header(frame, chunks[0], &agent_data, start_time);

            Self::render_tunnels(frame, chunks[1], &agent_data);

            render_stats_bar(frame, chunks[2], &stats);

            Self::render_logs(frame, chunks[3], &log_entries);

            render_help_bar(frame, chunks[4], quit_confirm);
        })?;

        Ok(())
    }

    fn render_tunnels(frame: &mut Frame, area: Rect, agent_data: &AgentData) {
        let block = Block::default()
            .title(" Tunnels ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        if agent_data.tunnels.is_empty() && agent_data.pending_tunnels.is_empty() {
            let msg = if agent_data.agent_id.is_empty() {
                "Loading tunnels..."
            } else {
                "No tunnels configured. Add one at playit.gg"
            };
            let paragraph = Paragraph::new(msg)
                .style(Style::default().fg(Color::Yellow))
                .block(block)
                .wrap(Wrap { trim: true });
            frame.render_widget(paragraph, area);
            return;
        }

        let items: Vec<ListItem> = agent_data
            .tunnels
            .iter()
            .map(|tunnel| {
                let (style, prefix) = if tunnel.is_disabled {
                    (Style::default().fg(Color::Red), "✗ ")
                } else {
                    (Style::default().fg(Color::Green), "● ")
                };

                let content = if let Some(reason) = &tunnel.disabled_reason {
                    format!(
                        "{}{} => (disabled: {})",
                        prefix, tunnel.display_address, reason
                    )
                } else {
                    format!(
                        "{}{} => {}",
                        prefix, tunnel.display_address, tunnel.destination
                    )
                };

                ListItem::new(content).style(style)
            })
            .chain(agent_data.pending_tunnels.iter().map(|pending| {
                let content = format!("◐ {} ({})", pending.id, pending.status_msg);
                ListItem::new(content).style(Style::default().fg(Color::Yellow))
            }))
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("▶ ");

        frame.render_widget(list, area);
    }

    fn render_logs(frame: &mut Frame, area: Rect, log_entries: &[LogEntry]) {
        let title = format!(" Service Logs ({}) [following] ", log_entries.len());

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner_height = area.height.saturating_sub(2) as usize;
        let start = log_entries.len().saturating_sub(inner_height);
        let visible_entries = log_entries.iter().skip(start).take(inner_height);

        let lines: Vec<Line> = visible_entries
            .map(|entry| {
                let level_style = match entry.level {
                    LogLevel::Error => Style::default().fg(Color::Red).bold(),
                    LogLevel::Warn => Style::default().fg(Color::Yellow).bold(),
                    LogLevel::Info => Style::default().fg(Color::Green),
                    LogLevel::Debug => Style::default().fg(Color::Blue),
                    LogLevel::Trace => Style::default().fg(Color::DarkGray),
                };

                Line::from(vec![
                    Span::styled(format!("[{}] ", level_label(&entry.level)), level_style),
                    Span::styled(
                        format!(
                            "{}: ",
                            entry.target.split("::").last().unwrap_or(&entry.target)
                        ),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(&entry.message),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_message_screen(frame: &mut Frame, area: Rect, message: &str, quit_confirm: bool) {
        use ratatui::layout::Alignment;

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(30),
                Constraint::Min(10),
                Constraint::Length(1),
            ])
            .split(area);

        let title_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta))
            .title(" playit.gg ");

        let lines: Vec<Line> = message
            .lines()
            .map(|line| {
                if line.starts_with("http://") || line.starts_with("https://") {
                    Line::from(Span::styled(
                        line,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ))
                } else if line.contains("https://") || line.contains("http://") {
                    let mut spans = Vec::new();
                    let mut remaining = line;
                    while let Some(pos) = remaining
                        .find("https://")
                        .or_else(|| remaining.find("http://"))
                    {
                        if pos > 0 {
                            spans.push(Span::styled(
                                &remaining[..pos],
                                Style::default().fg(Color::White),
                            ));
                        }

                        let url_end = remaining[pos..]
                            .find(' ')
                            .map(|offset| pos + offset)
                            .unwrap_or(remaining.len());
                        spans.push(Span::styled(
                            &remaining[pos..url_end],
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ));
                        remaining = &remaining[url_end..];
                    }
                    if !remaining.is_empty() {
                        spans.push(Span::styled(remaining, Style::default().fg(Color::White)));
                    }
                    Line::from(spans)
                } else {
                    Line::from(Span::styled(line, Style::default().fg(Color::White)))
                }
            })
            .collect();

        let paragraph = Paragraph::new(lines)
            .block(title_block)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, chunks[1]);
        render_help_bar(frame, chunks[2], quit_confirm);
    }
}

fn status_message(status: &ServiceStatus) -> Option<String> {
    if matches!(
        status.phase,
        playit_ipc::model::ServicePhase::DisabledOverLimit
    ) {
        return Some(format!(
            "{}\n{}",
            agent_over_limit_title(),
            agent_over_limit_guidance()
        ));
    }

    if let Some(error) = &status.last_error {
        return Some(format!(
            "playit service status: {} ({})",
            service_phase_label(status),
            error.message
        ));
    }

    if matches!(status.phase, playit_ipc::model::ServicePhase::Running) {
        None
    } else {
        Some(format!(
            "playit service status: {}",
            service_phase_label(status)
        ))
    }
}

fn service_phase_label(status: &ServiceStatus) -> &'static str {
    match status.phase {
        playit_ipc::model::ServicePhase::WaitingForSecret => "waiting for secret",
        playit_ipc::model::ServicePhase::HasInvalidSecret => "invalid secret",
        playit_ipc::model::ServicePhase::DisabledOverLimit => "disabled over limit",
        playit_ipc::model::ServicePhase::Starting => "starting",
        playit_ipc::model::ServicePhase::Running => "running",
        playit_ipc::model::ServicePhase::Stopping => "stopping",
        playit_ipc::model::ServicePhase::Error => "error",
    }
}

fn agent_over_limit_guidance() -> String {
    format!(
        "Delete unused agents: {ACCOUNT_AGENTS_URL}\nIncrease your agent limit: {ACCOUNT_UPGRADE_URL}"
    )
}

fn agent_over_limit_title() -> &'static str {
    "The playit service cannot start because this account is over the agent limit."
}

fn level_label(level: &LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "TRACE",
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERROR",
    }
}

impl From<ServiceAgentState> for AgentData {
    fn from(data: ServiceAgentState) -> Self {
        Self {
            version: data.version,
            tunnels: data
                .tunnels
                .into_iter()
                .map(|tunnel| TunnelInfo {
                    display_address: tunnel.display_address,
                    destination: tunnel.destination,
                    is_disabled: tunnel.is_disabled,
                    disabled_reason: tunnel.disabled_reason,
                })
                .collect(),
            pending_tunnels: data
                .pending_tunnels
                .into_iter()
                .map(|pending| PendingTunnelInfo {
                    id: pending.id,
                    status_msg: pending.status_msg,
                })
                .collect(),
            notices: data
                .notices
                .into_iter()
                .map(|notice| NoticeInfo {
                    priority: notice.priority,
                    message: notice.message,
                    resolve_link: notice.resolve_link,
                })
                .collect(),
            account_status: match data.account_status {
                ServiceAccountStatus::Guest => AccountStatusInfo::Guest,
                ServiceAccountStatus::EmailNotVerified => AccountStatusInfo::EmailNotVerified,
                ServiceAccountStatus::Verified => AccountStatusInfo::Verified,
                ServiceAccountStatus::Unknown => AccountStatusInfo::Unknown,
            },
            agent_id: data.agent_id,
            login_link: data.login_link,
            start_time: data.start_time,
        }
    }
}

impl From<ServiceConnectionStats> for ConnectionStats {
    fn from(stats: ServiceConnectionStats) -> Self {
        Self {
            bytes_in: stats.bytes_in,
            bytes_out: stats.bytes_out,
            active_tcp: stats.active_tcp,
            active_udp: stats.active_udp,
        }
    }
}

impl Drop for TuiApp {
    fn drop(&mut self) {
        let _ = self.restore_terminal();
    }
}
