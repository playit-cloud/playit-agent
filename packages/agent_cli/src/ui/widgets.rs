use playit_agent_core::utils::now_milli;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::tui_app::{AccountStatusInfo, AgentData, ConnectionStats};

/// Format bytes into a human-readable string
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Format duration in seconds to a human-readable string
fn format_uptime(start_time: u64) -> String {
    let elapsed_ms = now_milli().saturating_sub(start_time);
    let elapsed_secs = elapsed_ms / 1000;

    let hours = elapsed_secs / 3600;
    let minutes = (elapsed_secs % 3600) / 60;
    let seconds = elapsed_secs % 60;

    if hours > 0 {
        format!("{}h {:02}m {:02}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {:02}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

/// Render the header bar with version, uptime, and connection status
pub fn render_header(frame: &mut Frame, area: Rect, agent_data: &AgentData, start_time: u64) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Percentage(30),
            Constraint::Percentage(30),
        ])
        .split(area);

    // Version and title
    let version = if agent_data.version.is_empty() {
        env!("CARGO_PKG_VERSION").to_string()
    } else {
        agent_data.version.clone()
    };

    let title_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));

    let title = Paragraph::new(Line::from(vec![
        Span::styled("playit", Style::default().fg(Color::Magenta).bold()),
        Span::styled(format!(" v{}", version), Style::default().fg(Color::White)),
    ]))
    .block(title_block)
    .alignment(Alignment::Left);

    frame.render_widget(title, chunks[0]);

    // Uptime
    let uptime_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let uptime = Paragraph::new(Line::from(vec![
        Span::styled("⏱ ", Style::default().fg(Color::Cyan)),
        Span::styled(format_uptime(start_time), Style::default().fg(Color::White)),
    ]))
    .block(uptime_block)
    .alignment(Alignment::Center);

    frame.render_widget(uptime, chunks[1]);

    // Account status
    let (status_text, status_color) = match &agent_data.account_status {
        AccountStatusInfo::Verified => ("● Verified", Color::Green),
        AccountStatusInfo::Guest => ("○ Guest", Color::Yellow),
        AccountStatusInfo::EmailNotVerified => ("◐ Email Not Verified", Color::Yellow),
        AccountStatusInfo::Unknown => ("? Connecting...", Color::DarkGray),
    };

    let status_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let status = Paragraph::new(Span::styled(status_text, Style::default().fg(status_color)))
        .block(status_block)
        .alignment(Alignment::Right);

    frame.render_widget(status, chunks[2]);
}

/// Render the stats bar showing connection statistics
pub fn render_stats_bar(frame: &mut Frame, area: Rect, stats: &ConnectionStats) {
    let block = Block::default()
        .title(" Statistics ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .margin(1)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(block.inner(area));

    frame.render_widget(block, area);

    // Bytes In
    let bytes_in = Paragraph::new(Line::from(vec![
        Span::styled("↓ In: ", Style::default().fg(Color::Green)),
        Span::styled(format_bytes(stats.bytes_in), Style::default().fg(Color::White)),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(bytes_in, chunks[0]);

    // Bytes Out
    let bytes_out = Paragraph::new(Line::from(vec![
        Span::styled("↑ Out: ", Style::default().fg(Color::Blue)),
        Span::styled(format_bytes(stats.bytes_out), Style::default().fg(Color::White)),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(bytes_out, chunks[1]);

    // TCP Connections
    let tcp = Paragraph::new(Line::from(vec![
        Span::styled("TCP: ", Style::default().fg(Color::Cyan)),
        Span::styled(stats.active_tcp.to_string(), Style::default().fg(Color::White)),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(tcp, chunks[2]);

    // UDP Flows
    let udp = Paragraph::new(Line::from(vec![
        Span::styled("UDP: ", Style::default().fg(Color::Magenta)),
        Span::styled(stats.active_udp.to_string(), Style::default().fg(Color::White)),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(udp, chunks[3]);
}

/// Render the help bar with keybindings
pub fn render_help_bar(frame: &mut Frame, area: Rect, quit_confirm: bool) {
    let help_text = if quit_confirm {
        Line::from(vec![
            Span::styled("Quit? ", Style::default().fg(Color::Yellow).bold()),
            Span::styled("[y]", Style::default().fg(Color::Green).bold()),
            Span::raw(" Yes  "),
            Span::styled("[n]", Style::default().fg(Color::Red).bold()),
            Span::raw(" No"),
        ])
    } else {
        Line::from(vec![
            Span::styled("j/k", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(" Scroll  "),
            Span::styled("Tab", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(" Switch Panel  "),
            Span::styled("g/G", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(" Top/Bottom  "),
            Span::styled("q", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(" Quit"),
        ])
    };

    let help = Paragraph::new(help_text)
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);

    frame.render_widget(help, area);
}
