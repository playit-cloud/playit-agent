use std::collections::VecDeque;
use std::io::Stdout;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crossterm::event;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use tokio::sync::RwLock;
use tui::{Frame, Terminal};
use tui::backend::{Backend, CrosstermBackend};
use tui::layout::{Alignment, Constraint, Rect};
use tui::style::{Color, Modifier, Style};
use tui::text::{Span, Spans, Text};
use tui::widgets::{BarChart, Block, Borders, BorderType, Cell, Paragraph, Row, Table, Wrap};

use playit_agent_common::agent_config::{AgentConfig, get_match_ip, PortMappingConfig};
use playit_agent_core::agent_state::AgentState;
use playit_agent_core::tcp_client::TcpClients;

use crate::logging::LogReader;

pub struct GraphicInterface {
    state: Arc<RwLock<GraphicState>>,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    browser_opened: Option<String>,
}

pub enum GraphicState {
    Loading { message: String },
    LinkAgent { url: String },
    Notice(Notice),
    Connected(Connected),
}

impl GraphicState {
    pub fn connected_mut(&mut self) -> Option<&mut Connected> {
        match self {
            GraphicState::Connected(connected) => Some(connected),
            _ => None
        }
    }
}

pub struct Notice {
    pub message: String,
    pub url: String,
    pub important: bool,
}

pub struct Connected {
    pub focused: ConnectedElement,
    pub ping_samples: VecDeque<u64>,
    pub config: Arc<AgentConfig>,
    pub log_reader: LogReader,
    pub logs: VecDeque<String>,
    pub selected_tunnel_pos: usize,
    pub agent_state: Arc<AgentState>,
    pub tcp_clients: Arc<TcpClients>,
}

#[derive(PartialEq, Eq, Copy, Clone)]
pub enum ConnectedElement {
    Overview,
    Tunnels,
    Network,
    Logs,
}

impl Default for ConnectedElement {
    fn default() -> Self {
        ConnectedElement::Network
    }
}

#[derive(Debug)]
pub enum TunnelStatus {
    SettingUp,
    Authenticating,
    Active,
    Removing,
}

impl Drop for GraphicInterface {
    fn drop(&mut self) {
        disable_raw_mode().unwrap();
    }
}

impl GraphicInterface {
    pub fn new() -> Result<Self, ()> {
        if enable_raw_mode().is_err() {
            return Err(());
        }

        let stdout = std::io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).unwrap();

        Ok(GraphicInterface {
            state: Arc::new(RwLock::new(GraphicState::Loading { message: "Preparing".to_string() })),
            terminal,
            browser_opened: None,
        })
    }

    pub fn state(&self) -> Arc<RwLock<GraphicState>> {
        self.state.clone()
    }

    pub async fn run(mut self) {
        loop {
            self.draw().await;

            let event = tokio::task::spawn_blocking(|| {
                if event::poll(Duration::from_millis(300)).unwrap_or(false) {
                    event::read().ok()
                } else {
                    None
                }
            }).await.unwrap();

            match event {
                Some(Event::Key(key)) => {
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        print!("\x1B[2J\x1B[1;1H");
                        break;
                    }

                    let mut write = self.state.write().await;

                    if let GraphicState::Connected(connected) = &mut *write {
                        /* select tab */
                        let target_focus = match key.code {
                            KeyCode::Char('o') | KeyCode::Char('0') | KeyCode::Char('1') => Some(ConnectedElement::Overview),
                            KeyCode::Char('t') | KeyCode::Char('2') => Some(ConnectedElement::Tunnels),
                            KeyCode::Char('n') | KeyCode::Char('3') => Some(ConnectedElement::Network),
                            KeyCode::Char('l') | KeyCode::Char('4') => Some(ConnectedElement::Logs),
                            _ => None,
                        };

                        if let Some(focus) = target_focus {
                            connected.focused = focus;
                        }

                        if connected.focused == ConnectedElement::Tunnels {
                            let direction: isize = match key.code {
                                KeyCode::Left | KeyCode::Up | KeyCode::Char('k') => -1,
                                KeyCode::Right | KeyCode::Down | KeyCode::Char('j') => 1,
                                _ => 0
                            };

                            let tunnel_len = connected.config.mappings.len();
                            if tunnel_len > 0 {
                                connected.selected_tunnel_pos = (((connected.selected_tunnel_pos + tunnel_len) as isize + direction) as usize) % tunnel_len;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub async fn draw(&mut self) {
        {
            let mut lock = self.state.write().await;
            if let GraphicState::Connected(connected) = &mut *lock {
                while let Some(line) = connected.log_reader.try_read() {
                    connected.logs.push_front(line);
                }
                connected.logs.truncate(1024);
            }
        }

        {
            let state = self.state.read().await;
            {
                self.terminal.autoresize().unwrap();
                let mut frame = self.terminal.get_frame();
                state.render(&mut frame).await;
                self.terminal.draw(|_| {}).unwrap();
            }

            /* open web browser to notice */
            let open_url = match &*state {
                GraphicState::LinkAgent { url } => Some(url),
                GraphicState::Notice(notice) if notice.important => Some(&notice.url),
                _ => None,
            };

            if let Some(notice_url) = open_url {
                match &self.browser_opened {
                    Some(url) if notice_url.eq(url) => {}
                    _ => {
                        if let Err(error) = webbrowser::open(&notice_url) {
                            tracing::error!(?error, "failed to open web browser");
                        }
                        self.browser_opened = Some(notice_url.clone());
                    }
                }
            }
        }
    }
}

impl GraphicState {
    async fn render<B: Backend>(&self, frame: &mut Frame<'_, B>) {
        match self {
            GraphicState::Loading { message } => self.loading(frame, message),
            GraphicState::LinkAgent { url } => self.link_agent(frame, url),
            GraphicState::Connected(connected) => self.connected(frame, connected).await,
            GraphicState::Notice(notice) => self.notice(frame, notice),
        }
    }

    fn loading<B: Backend>(&self, frame: &mut Frame<B>, message: &str) {
        let size = frame.size();

        let paragraph = Paragraph::new(Text {
            lines: vec![
                Spans(vec![Span::styled(format!("playit.gg agent version: {}", env!("CARGO_PKG_VERSION")), Style::default().add_modifier(Modifier::BOLD))]),
                Spans(vec![
                    Span::styled("Status: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(message, Style::default()),
                ]),
            ]
        }).wrap(Wrap { trim: true });

        let default_style = Style::default().bg(Color::Black).fg(Color::White);
        frame.render_widget(Block::default().style(default_style), size);

        let y_offset = (size.height / 2).max(1);
        frame.render_widget(
            paragraph
                .style(default_style)
                .alignment(Alignment::Center),
            Rect::new(0, y_offset - 1, size.width, 2),
        );
    }

    fn notice<B: Backend>(&self, frame: &mut Frame<B>, notice: &Notice) {
        let size = frame.size();

        let line_count;
        let paragraph = if size.height >= 3 {
            line_count = 3;

            let mut lines = vec![
                Spans(vec![Span::styled(format!("playit.gg agent version: {}", env!("CARGO_PKG_VERSION")), Style::default())]),
                Spans(vec![Span::styled(&notice.message, Style::default().add_modifier(Modifier::BOLD))]),
            ];

            if notice.url.len() + 6 < (size.width as usize) {
                lines.push(
                    Spans(vec![
                        Span::styled("Visit: ", Style::default().add_modifier(Modifier::BOLD)),
                        Span::styled(format!("{:?}", notice.url), Style::default().add_modifier(Modifier::UNDERLINED)),
                    ])
                );
            }

            Paragraph::new(Text {
                lines,
            }).wrap(Wrap { trim: true }).alignment(Alignment::Center)
        } else {
            line_count = 2;

            Paragraph::new(Text {
                lines: vec![
                    Spans(vec![
                        Span::styled(format!("playit.gg (v{}) ", env!("CARGO_PKG_VERSION")), Style::default().add_modifier(Modifier::BOLD)),
                        Span::styled(&notice.message, Style::default()),
                    ]),
                    Spans(vec![Span::styled(&notice.url, Style::default().add_modifier(Modifier::UNDERLINED))]),
                ]
            }).wrap(Wrap { trim: true })
        };

        let default_style = Style::default().bg(Color::Black).fg(Color::White);
        frame.render_widget(Block::default().style(default_style), size);

        let y_offset = (size.height / 2).max(line_count / 2);
        frame.render_widget(
            paragraph.style(default_style),
            Rect::new(0, y_offset - line_count / 2, size.width, line_count),
        );
    }

    async fn connected<B: Backend>(&self, frame: &mut Frame<'_, B>, connected: &Connected) {
        let size = frame.size();

        let current_latency = connected.ping_samples.get(0).map(|v| format!("{}ms", v)).unwrap_or("?ms".to_string());

        let mut height_used = 0;

        if size.height > 10 {
            let mut title = Paragraph::new(Text {
                lines: vec![
                    Spans(vec![
                        Span::styled(" playit.gg program ", Style::default()),
                        Span::styled(format!("(v{})", env!("CARGO_PKG_VERSION")), Style::default().add_modifier(Modifier::BOLD)),
                    ]),
                ]
            }).style(Style::default().bg(Color::Black).fg(Color::White));

            if size.height > 20 {
                title = title.block(Block::default().borders(Borders::BOTTOM).border_type(BorderType::Thick).border_style(Style::default().bg(Color::Black).fg(Color::White)));
                frame.render_widget(title, Rect::new(0, 0, size.width, 2));
                height_used += 2;
            } else {
                frame.render_widget(title, Rect::new(0, 0, size.width, 1));
                height_used += 1;
            }
        }

        {
            let mut tab_elements = vec![
                Span::styled(" (o) Overview ", connected.tab_style(ConnectedElement::Overview)),
                Span::styled(format!(" (t) Tunnels [{}] ", connected.config.mappings.len()), connected.tab_style(ConnectedElement::Tunnels)),
                Span::styled(format!(" (n) Network [{}] ", current_latency), connected.tab_style(ConnectedElement::Network)),
                Span::styled(" (l) Logs ", connected.tab_style(ConnectedElement::Logs)),
            ];

            let mut height = 0;

            let mut tabs = if size.width < 70 {
                if size.width < 35 {
                    height += 4;

                    Paragraph::new(Text {
                        lines: tab_elements.into_iter().map(|t| Spans(vec![t])).collect()
                    })
                } else {
                    height += 2;

                    Paragraph::new(Text {
                        lines: vec![
                            Spans(vec![tab_elements.remove(0), tab_elements.remove(0)]),
                            Spans(vec![tab_elements.remove(0), tab_elements.remove(0)]),
                        ]
                    })
                }
            } else {
                height += 1;

                Paragraph::new(Text {
                    lines: vec![
                        Spans(tab_elements),
                    ]
                })
            };

            tabs = tabs.style(Style::default().bg(Color::Black));

            if height_used + height + 10 < size.height {
                tabs = tabs.block(Block::default().borders(Borders::BOTTOM).border_type(BorderType::Double));
                height += 1;
            }

            if height_used + height + 2 < size.height {
                frame.render_widget(tabs, Rect::new(0, height_used, size.width, height));
                height_used += height;
            }
        }

        let mut body_rect = Rect::new(0, height_used, size.width, size.height - height_used);

        let udp_is_setup = {
            let lock = connected.agent_state.udp_tunnel.read().await;
            match &*lock {
                Some(v) => v.is_active(),
                None => false,
            }
        };

        match connected.focused {
            ConnectedElement::Tunnels => {
                let mut tunnels: Vec<&PortMappingConfig> = connected.config.mappings.iter().collect();

                if tunnels.len() > 0 {
                    tunnels.sort_by(|a, b| SocketAddr::new(a.tunnel_ip, a.tunnel_from_port).cmp(&SocketAddr::new(b.tunnel_ip, b.tunnel_from_port)));

                    let mut addr_len = 0;

                    let mut rows = Vec::new();
                    let mut pos = 0;
                    for tunnel in &tunnels {
                        addr_len = addr_len.max(tunnel.preview_address.len());

                        let mut row = Row::new(vec![
                            Cell::from(Text::raw(&tunnel.preview_address)).style(Style::default().fg(Color::Rgb(255, 121, 25)).add_modifier(Modifier::BOLD)),
                            Cell::from("=>"),
                            Cell::from(Text::raw(SocketAddr::new(tunnel.local_ip, tunnel.local_port).to_string())).style(Style::default().fg(Color::White)),
                        ]);

                        if pos == connected.selected_tunnel_pos {
                            row = row.style(Style::default().bg(Color::Blue));
                        }
                        pos += 1;

                        rows.push(row);
                    }

                    if connected.selected_tunnel_pos > 2 && rows.len() >= 3 {
                        rows.drain(0..connected.selected_tunnel_pos - 1);
                    }

                    let widths = [Constraint::Length(addr_len as _), Constraint::Length(2), Constraint::Length(21)];
                    let table = Table::new(rows).block(
                        Block::default()
                            .title("active tunnels")
                            .style(Style::default().bg(Color::Black))
                            .borders(Borders::BOTTOM)
                    )
                        .column_spacing(1)
                        .widths(&widths)
                        .highlight_style(Style::default().bg(Color::Blue));

                    if body_rect.height > 5 {
                        frame.render_widget(table, Rect::new(body_rect.x, body_rect.y, body_rect.width, 5));

                        body_rect.height -= 5;
                        body_rect.y += 5;
                    } else {
                        frame.render_widget(table, body_rect);
                    }

                    let selected_tunnel = tunnels[connected.selected_tunnel_pos.min(tunnels.len() - 1)];

                    let tunnel_status = {
                        let claims = connected.agent_state.port_claims.read().await;
                        let mut found = TunnelStatus::SettingUp;

                        let search_ip = get_match_ip(selected_tunnel.tunnel_ip);
                        for claim in claims.current() {
                            if get_match_ip(claim.tunnel_ip) == search_ip
                                && claim.from_port <= selected_tunnel.tunnel_from_port
                                && claim.to_port >= selected_tunnel.tunnel_to_port {
                                if claim.should_remove {
                                    found = TunnelStatus::Removing;
                                } else if claim.last_ack != 0 {
                                    if selected_tunnel.proto.has_udp() && !udp_is_setup {
                                        found = TunnelStatus::Authenticating
                                    } else {
                                        found = TunnelStatus::Active
                                    }
                                }

                                break;
                            }
                        }

                        found
                    };

                    let tcp_clients = if selected_tunnel.proto.has_tcp() {
                        connected.tcp_clients.client_for_tunnel(
                            selected_tunnel.tunnel_ip,
                            selected_tunnel.tunnel_from_port,
                            selected_tunnel.tunnel_to_port,
                        ).await
                    } else {
                        vec![]
                    };

                    let udp_clients = if selected_tunnel.proto.has_udp() {
                        let lock = connected.agent_state.udp_tunnel.read().await;
                        match &*lock {
                            Some(udp_tunnel) => udp_tunnel.get_udp_clients(
                                selected_tunnel.tunnel_ip,
                                selected_tunnel.tunnel_from_port,
                                selected_tunnel.tunnel_to_port,
                            ).await,
                            None => vec![]
                        }
                    } else {
                        vec![]
                    };

                    let mut lines = vec![
                        Spans(vec![
                            Span::styled("Tunnel Status: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(format!("{:?}", tunnel_status)),
                        ]),
                        Spans(vec![
                            Span::styled("Tunnel Name: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(selected_tunnel.name.as_ref().map(|v| v.as_str()).unwrap_or("untitled")),
                        ]),
                        Spans(vec![
                            Span::styled("Tunnel Port Range: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(format!("{} to {}", selected_tunnel.tunnel_from_port, selected_tunnel.tunnel_to_port)),
                        ]),
                        Spans(vec![
                            Span::styled("Local Addr: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(format!("{}/{:?}", SocketAddr::new(selected_tunnel.local_ip, selected_tunnel.local_port), selected_tunnel.proto)),
                        ]),
                        Spans(vec![
                            Span::styled("Connections: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw((tcp_clients.len() + udp_clients.len()).to_string()),
                        ]),
                    ];

                    for client in &tcp_clients {
                        lines.push(Spans(vec![
                            Span::raw(client.client_addr.to_string()),
                            Span::styled(" <=> ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(client.tunnel_addr.to_string()),
                            Span::styled(" (playit) ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(client.client_peer_addr.to_string()),
                            Span::styled(" <=> ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(client.client_local_addr.to_string()),
                            Span::styled(" (agent) ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(client.host_local_addr.to_string()),
                            Span::styled(" <=> ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(client.host_peer_addr.to_string()),
                        ]));
                    }

                    for client in &udp_clients {
                        lines.push(Spans(vec![
                            Span::raw(client.flow_key.client_addr.to_string()),
                            Span::styled(" <=> ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(client.flow_key.tunnel_addr.to_string()),
                            Span::styled(" (playit) ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(client.udp_tunnel_addr.to_string()),
                            Span::styled(" <=> ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(client.tunnel_udp.local_addr().unwrap().to_string()),
                            Span::styled(" (agent) ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(client.host_udp.local_addr().unwrap().to_string()),
                            Span::styled(" <=> ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(client.host_forward_addr.to_string()),
                        ]));
                    }

                    let status = Paragraph::new(Text {
                        lines,
                    }).wrap(Wrap { trim: true });

                    frame.render_widget(status, body_rect);
                } else {
                    let vertical_offset = body_rect.y + (body_rect.height / 2).max(1) - 1;

                    frame.render_widget(
                        Paragraph::new(Text {
                            lines: vec![
                                Spans(vec![
                                    Span::raw("no tunnels setup"),
                                ]),
                                Spans(vec![
                                    Span::raw("visit "),
                                    Span::styled("https://playit.gg/account/tunnels", Style::default().add_modifier(Modifier::UNDERLINED)),
                                    Span::raw(" to add a tunnel"),
                                ]),
                            ]
                        }).wrap(Wrap { trim: true }).alignment(Alignment::Center),
                        Rect::new(body_rect.x, vertical_offset, body_rect.width, body_rect.height - vertical_offset),
                    );
                }
            }
            ConnectedElement::Network => {
                let mut data = Vec::new();
                let bars_num = body_rect.width / 4;

                for sample in &connected.ping_samples {
                    data.insert(0,("", *sample));
                    if data.len() >= bars_num.into() {
                        data.remove(0);
                    }

                }

                let mut latency_graph = BarChart::default()
                    .bar_width(3)
                    .style(Style::default().bg(Color::Black))
                    .data(&data)
                    .value_style(Style::default().bg(Color::Black));

                if 10 < size.height - height_used {
                    latency_graph = latency_graph.block(
                        Block::default()
                            .title("latency to tunnel server")
                            .borders(Borders::ALL)
                    );
                }

                frame.render_widget(latency_graph, body_rect);
            }
            ConnectedElement::Logs => {
                let paragraph = {
                    let mut lines = Vec::new();
                    for line in connected.logs.iter().take(body_rect.height as _) {
                        lines.push(Spans(vec![Span::raw(line)]));
                    }

                    Paragraph::new(Text {
                        lines
                    }).wrap(Wrap { trim: true })
                };

                frame.render_widget(paragraph, body_rect);
            }
            ConnectedElement::Overview => {
                let is_authenticated = connected.agent_state.authenticate_times.is_fresh(5_000);
                let is_alive = connected.agent_state.keep_alive_times.is_fresh(5_000);
                let ports_alive = connected.agent_state.port_claim_times.is_fresh(5_000);

                let latency = connected.agent_state.latency.load(Ordering::SeqCst);
                let connected_server_id = connected.agent_state.connected_server_id.load(Ordering::SeqCst);

                let tcp_clients = connected.tcp_clients.client_count().await;
                let udp_clients = {
                    let lock = connected.agent_state.udp_tunnel.read().await;
                    match &*lock {
                        Some(v) => v.client_count().await,
                        None => 0,
                    }
                };

                let paragraph = Paragraph::new(Text {
                    lines: vec![
                        Spans(vec![
                            Span::styled("Visit ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::styled("https://playit.gg/account", Style::default().add_modifier(Modifier::UNDERLINED)),
                            Span::raw(" to manage your account"),
                        ]),
                        Spans(vec![
                            Span::styled("Authenticated: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(is_authenticated.to_string()),
                        ]),
                        Spans(vec![
                            Span::styled("Connection Alive: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(is_alive.to_string()),
                        ]),
                        Spans(vec![
                            Span::styled("Tunnels Setup: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(ports_alive.to_string()),
                        ]),
                        Spans(vec![
                            Span::styled("Latest Latency: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(format!("{} ms", latency)),
                        ]),
                        Spans(vec![
                            Span::styled("Connected Tunnel Server ID: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(connected_server_id.to_string()),
                        ]),
                        Spans(vec![
                            Span::styled("TCP Client Count: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(tcp_clients.to_string()),
                        ]),
                        Spans(vec![
                            Span::styled("UDP Client Count: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(udp_clients.to_string()),
                        ]),
                    ]
                }).wrap(Wrap { trim: true });

                frame.render_widget(paragraph, body_rect);
            }
        }
    }

    fn link_agent<B: Backend>(&self, frame: &mut Frame<B>, url: &str) {
        let size = frame.size();

        let paragraph = if size.height < 3 {
            Paragraph::new(Text {
                lines: vec![
                    Spans(vec![
                        Span::styled("Visit: ", Style::default()),
                        Span::styled(url, Style::default().add_modifier(Modifier::BOLD).add_modifier(Modifier::UNDERLINED)),
                    ]),
                ]
            }).wrap(Wrap { trim: true })
        } else {
            let code = last_split(url.split("/")).unwrap();

            Paragraph::new(Text {
                lines: vec![
                    Spans(vec![
                        Span::styled("Visit web page: ", Style::default()),
                        Span::styled(url, Style::default().add_modifier(Modifier::BOLD).add_modifier(Modifier::UNDERLINED)),
                    ]),
                    Spans(vec![
                        Span::styled("Or enter code:  ", Style::default()),
                        Span::styled(
                            code,
                            Style::default()
                                .add_modifier(Modifier::BOLD)
                                .add_modifier(Modifier::UNDERLINED),
                        ),
                    ]),
                    Spans(vec![
                        Span::styled(format!("Setup playit.gg (v{})", env!("CARGO_PKG_VERSION")), Style::default().add_modifier(Modifier::BOLD)),
                    ]),
                ]
            }).wrap(Wrap { trim: false })
        };

        frame.render_widget(
            paragraph
                .style(Style::default().bg(Color::Black).fg(Color::White))
                .alignment(Alignment::Left),
            size,
        );
    }
}

fn last_split<'a>(mut line: std::str::Split<'a, &'a str>) -> Option<&'a str> {
    let mut last = line.next()?;

    loop {
        let next = line.next();
        match next {
            None => return Some(last),
            Some(v) => last = v,
        }
    }
}

impl Connected {
    fn tab_style(&self, elem: ConnectedElement) -> Style {
        if self.focused == elem {
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
        } else {
            Style::default()
                .fg(Color::White)
                .bg(Color::Black)
        }
    }
}