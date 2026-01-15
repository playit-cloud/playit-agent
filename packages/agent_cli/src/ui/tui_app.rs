use std::io::{self, stdout, Stdout};
use std::sync::Arc;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use super::log_capture::{LogCapture, LogEntry, LogLevel};
use super::widgets::{render_header, render_help_bar, render_stats_bar};
use super::UISettings;
use crate::signal_handle::get_signal_handle;
use crate::CliError;

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

/// Which panel is currently focused
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FocusedPanel {
    Tunnels,
    Logs,
}

/// UI mode for TuiApp
#[derive(Clone, Debug, PartialEq)]
pub enum TuiMode {
    /// Setup mode - showing a message (e.g., claim URL)
    Setup { message: String },
    /// Running mode - showing tunnels and stats
    Running,
}

/// Main TUI application state
pub struct TuiApp {
    settings: UISettings,
    log_capture: Arc<LogCapture>,
    agent_data: AgentData,
    stats: ConnectionStats,

    // UI state
    mode: TuiMode,
    focused_panel: FocusedPanel,
    tunnel_list_state: ListState,
    log_scroll: usize,
    log_follow: bool, // Auto-scroll logs when at bottom
    should_quit: bool,
    quit_confirm: bool,

    // Terminal
    terminal: Option<Terminal<CrosstermBackend<Stdout>>>,
}

impl TuiApp {
    pub fn new(settings: UISettings) -> Self {
        TuiApp {
            settings,
            log_capture: LogCapture::new(500),
            agent_data: AgentData::default(),
            stats: ConnectionStats::default(),
            mode: TuiMode::Setup { message: "Initializing...".to_string() },
            focused_panel: FocusedPanel::Tunnels,
            tunnel_list_state: ListState::default(),
            log_scroll: 0,
            log_follow: true, // Start with follow mode enabled
            should_quit: false,
            quit_confirm: false,
            terminal: None,
        }
    }

    pub fn log_capture(&self) -> Arc<LogCapture> {
        self.log_capture.clone()
    }

    pub fn update_agent_data(&mut self, data: AgentData) {
        self.agent_data = data;
        // Switch to running mode when we get agent data
        self.mode = TuiMode::Running;
    }

    pub fn update_stats(&mut self, stats: ConnectionStats) {
        self.stats = stats;
    }

    /// Set the setup message to display
    pub fn set_setup_message(&mut self, message: String) {
        self.mode = TuiMode::Setup { message };
    }

    /// Switch to running mode
    pub fn set_running_mode(&mut self) {
        self.mode = TuiMode::Running;
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
        disable_raw_mode()?;
        if let Some(ref mut terminal) = self.terminal {
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
            terminal.show_cursor()?;
        }
        Ok(())
    }

    /// Initialize the TUI (call once before tick())
    pub fn init(&mut self) -> Result<(), CliError> {
        if self.terminal.is_none() {
            self.init_terminal().map_err(CliError::RenderError)?;
        }
        Ok(())
    }

    /// Shutdown the TUI (call when done)
    pub fn shutdown(&mut self) -> Result<(), CliError> {
        self.restore_terminal().map_err(CliError::RenderError)
    }

    /// Check if the TUI should quit
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Run one iteration of the TUI (draw + handle events)
    /// Returns Ok(true) if should continue, Ok(false) if should quit
    pub fn tick(&mut self) -> Result<bool, CliError> {
        // Initialize if not already
        if self.terminal.is_none() {
            self.init()?;
        }

        // Draw the UI
        self.draw().map_err(CliError::RenderError)?;

        // Handle events with a short timeout to allow for async updates
        if event::poll(Duration::from_millis(50)).map_err(CliError::RenderError)? {
            if let Event::Key(key) = event::read().map_err(CliError::RenderError)? {
                self.handle_key_event(key);
            }
        }

        // Check for signal close request
        let signal = get_signal_handle();
        if signal.is_confirming_close() && !self.quit_confirm {
            self.quit_confirm = true;
        }

        // Return whether to continue
        Ok(!self.should_quit)
    }

    /// Run the TUI event loop (blocking)
    pub async fn run(&mut self) -> Result<(), CliError> {
        self.init()?;

        loop {
            if !self.tick()? {
                break;
            }
            // Yield to allow other tasks to run
            tokio::task::yield_now().await;
        }

        self.shutdown()?;
        Ok(())
    }

    fn handle_key_event(&mut self, key: KeyEvent) {
        // Handle quit confirmation
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
            // Quit
            KeyCode::Char('q') => {
                self.quit_confirm = true;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit_confirm = true;
            }

            // Navigation
            KeyCode::Tab => {
                self.focused_panel = match self.focused_panel {
                    FocusedPanel::Tunnels => FocusedPanel::Logs,
                    FocusedPanel::Logs => FocusedPanel::Tunnels,
                };
            }

            // Scrolling
            KeyCode::Char('j') | KeyCode::Down => self.scroll_down(),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_up(),
            KeyCode::Char('g') => self.scroll_to_top(),
            KeyCode::Char('G') => self.scroll_to_bottom(),
            KeyCode::PageDown => {
                for _ in 0..10 {
                    self.scroll_down();
                }
            }
            KeyCode::PageUp => {
                for _ in 0..10 {
                    self.scroll_up();
                }
            }

            _ => {}
        }
    }

    fn scroll_down(&mut self) {
        match self.focused_panel {
            FocusedPanel::Tunnels => {
                let total = self.agent_data.tunnels.len();
                if total > 0 {
                    let i = match self.tunnel_list_state.selected() {
                        Some(i) => (i + 1).min(total - 1),
                        None => 0,
                    };
                    self.tunnel_list_state.select(Some(i));
                }
            }
            FocusedPanel::Logs => {
                let total = self.log_capture.len();
                if self.log_scroll < total.saturating_sub(1) {
                    self.log_scroll += 1;
                    // Re-enable follow if we scrolled to the bottom
                    if self.log_scroll >= total.saturating_sub(1) {
                        self.log_follow = true;
                    }
                }
            }
        }
    }

    fn scroll_up(&mut self) {
        match self.focused_panel {
            FocusedPanel::Tunnels => {
                let i = match self.tunnel_list_state.selected() {
                    Some(i) => i.saturating_sub(1),
                    None => 0,
                };
                self.tunnel_list_state.select(Some(i));
            }
            FocusedPanel::Logs => {
                self.log_scroll = self.log_scroll.saturating_sub(1);
                // Disable follow when scrolling up
                self.log_follow = false;
            }
        }
    }

    fn scroll_to_top(&mut self) {
        match self.focused_panel {
            FocusedPanel::Tunnels => {
                self.tunnel_list_state.select(Some(0));
            }
            FocusedPanel::Logs => {
                self.log_scroll = 0;
                // Disable follow when going to top
                self.log_follow = false;
            }
        }
    }

    fn scroll_to_bottom(&mut self) {
        match self.focused_panel {
            FocusedPanel::Tunnels => {
                let total = self.agent_data.tunnels.len();
                if total > 0 {
                    self.tunnel_list_state.select(Some(total - 1));
                }
            }
            FocusedPanel::Logs => {
                let total = self.log_capture.len();
                self.log_scroll = total.saturating_sub(1);
                // Enable follow when going to bottom
                self.log_follow = true;
            }
        }
    }

    fn draw(&mut self) -> io::Result<()> {
        let terminal = self.terminal.as_mut().unwrap();

        let mode = self.mode.clone();
        let agent_data = self.agent_data.clone();
        let stats = self.stats.clone();
        let start_time = agent_data.start_time;
        let focused_panel = self.focused_panel;
        let quit_confirm = self.quit_confirm;
        let log_entries = self.log_capture.get_entries();
        let log_follow = self.log_follow;

        // Auto-scroll to bottom if following logs
        let log_scroll = if log_follow {
            let total = log_entries.len();
            self.log_scroll = total.saturating_sub(1);
            self.log_scroll
        } else {
            self.log_scroll
        };

        let mut tunnel_list_state = self.tunnel_list_state.clone();

        terminal.draw(|frame| {
            let area = frame.area();

            match &mode {
                TuiMode::Setup { message } => {
                    // Render setup screen with centered message
                    Self::render_setup_screen(frame, area, message, quit_confirm);
                    return;
                }
                TuiMode::Running => {
                    // Normal running mode
                }
            }

            // Main layout: Header, Content, Stats, Logs, Help
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),  // Header
                    Constraint::Min(8),     // Tunnels
                    Constraint::Length(3),  // Stats
                    Constraint::Length(10), // Logs
                    Constraint::Length(1),  // Help bar
                ])
                .split(area);

            // Render header
            render_header(frame, chunks[0], &agent_data, start_time);

            // Render tunnel list
            Self::render_tunnels(
                frame,
                chunks[1],
                &agent_data,
                focused_panel == FocusedPanel::Tunnels,
                &mut tunnel_list_state,
            );

            // Render stats bar
            render_stats_bar(frame, chunks[2], &stats);

            // Render log panel
            Self::render_logs(
                frame,
                chunks[3],
                &log_entries,
                log_scroll,
                focused_panel == FocusedPanel::Logs,
                log_follow,
            );

            // Render help bar
            render_help_bar(frame, chunks[4], quit_confirm);
        })?;

        self.tunnel_list_state = tunnel_list_state;

        Ok(())
    }

    fn render_tunnels(
        frame: &mut Frame,
        area: Rect,
        agent_data: &AgentData,
        focused: bool,
        list_state: &mut ListState,
    ) {
        let border_style = if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let block = Block::default()
            .title(" Tunnels ")
            .borders(Borders::ALL)
            .border_style(border_style);

        if agent_data.tunnels.is_empty() && agent_data.pending_tunnels.is_empty() {
            let msg = if agent_data.agent_id.is_empty() {
                "No tunnels configured. Setting up..."
            } else {
                "No tunnels configured. Add tunnels at playit.gg"
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
                    format!("{}{} => {}", prefix, tunnel.display_address, tunnel.destination)
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

        frame.render_stateful_widget(list, area, list_state);
    }

    fn render_logs(
        frame: &mut Frame,
        area: Rect,
        log_entries: &[LogEntry],
        scroll: usize,
        focused: bool,
        following: bool,
    ) {
        let border_style = if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let title = if following {
            format!(" Logs ({}) [following] ", log_entries.len())
        } else {
            format!(" Logs ({}) ", log_entries.len())
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);

        let inner_height = area.height.saturating_sub(2) as usize;
        let start = scroll.min(log_entries.len().saturating_sub(inner_height));
        let visible_entries = log_entries
            .iter()
            .skip(start)
            .take(inner_height);

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
                    Span::styled(
                        format!("[{}] ", entry.level.as_str()),
                        level_style,
                    ),
                    Span::styled(
                        format!("{}: ", entry.target.split("::").last().unwrap_or(&entry.target)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(&entry.message),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_setup_screen(frame: &mut Frame, area: Rect, message: &str, quit_confirm: bool) {
        use ratatui::layout::Alignment;

        // Create a centered layout
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(30),
                Constraint::Min(10),
                Constraint::Length(1),
            ])
            .split(area);

        // Title block
        let title_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta))
            .title(" playit.gg ");

        // Parse message into lines and style URLs differently
        let lines: Vec<Line> = message
            .lines()
            .map(|line| {
                if line.starts_with("http://") || line.starts_with("https://") {
                    Line::from(Span::styled(
                        line,
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ))
                } else if line.contains("https://") || line.contains("http://") {
                    // Line contains a URL somewhere
                    let mut spans = Vec::new();
                    let mut remaining = line;
                    while let Some(pos) = remaining.find("https://").or_else(|| remaining.find("http://")) {
                        if pos > 0 {
                            spans.push(Span::styled(&remaining[..pos], Style::default().fg(Color::White)));
                        }
                        // Find end of URL (space or end of string)
                        let url_start = pos;
                        let url_end = remaining[pos..].find(' ').map(|p| pos + p).unwrap_or(remaining.len());
                        spans.push(Span::styled(
                            &remaining[url_start..url_end],
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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

        // Help bar
        render_help_bar(frame, chunks[2], quit_confirm);
    }

    /// Simple screen write for compatibility (used during setup)
    pub async fn write_screen<T: std::fmt::Display>(&mut self, content: T) {
        let signal = get_signal_handle();
        let exit_confirm = signal.is_confirming_close();

        if exit_confirm {
            match self
                .yn_question(
                    format!("{}\nClose requested, close program?", content),
                    Some(true),
                )
                .await
            {
                Ok(close) => {
                    if close {
                        std::process::exit(0);
                    } else {
                        signal.decline_close();
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "failed to ask close signal question");
                }
            }
            return;
        }

        // Set the setup message and render
        let message = content.to_string();
        tracing::info!("{}", message.lines().next().unwrap_or(""));
        self.set_setup_message(message);

        // Initialize terminal if not already done
        if self.terminal.is_none() {
            if let Err(e) = self.init_terminal() {
                tracing::error!(?e, "Failed to init terminal");
                return;
            }
        }

        // Draw the screen
        if let Err(e) = self.draw() {
            tracing::error!(?e, "Failed to draw screen");
        }

        // Handle any pending keyboard events (non-blocking)
        if let Ok(true) = event::poll(Duration::from_millis(10)) {
            if let Ok(Event::Key(key)) = event::read() {
                self.handle_key_event(key);
            }
        }

        // Check if quit was requested
        if self.should_quit {
            let _ = self.restore_terminal();
            std::process::exit(0);
        }
    }

    pub async fn yn_question<T: std::fmt::Display + Send + 'static>(
        &mut self,
        _question: T,
        default_yes: Option<bool>,
    ) -> Result<bool, CliError> {
        // For TUI mode, we use the quit confirm mechanism
        // For now, return the default if available
        if let Some(default) = default_yes {
            return Ok(default);
        }
        if let Some(auto) = self.settings.auto_answer {
            return Ok(auto);
        }
        Err(CliError::AnswerNotProvided)
    }
}

impl Drop for TuiApp {
    fn drop(&mut self) {
        let _ = self.restore_terminal();
    }
}
