use std::sync::Arc;
use std::time::Duration;

use crossterm::{event, execute};
use crossterm::event::{Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::Level;
use tui::{Frame, Terminal};
use tui::backend::{Backend, CrosstermBackend};
use tui::layout::{Alignment, Corner, Rect};
use tui::style::{Color, Modifier, Style};
use tui::text::{Span};
use tui::widgets::{Block, Borders, BorderType, Gauge, List, ListItem, Paragraph, Wrap};

use agent::agent_config::{AgentConfigStatus, ManagedAgentConfig, prepare_config};
use agent::application::{AgentState, Application, RunningState};
use agent::events::PlayitEvents;
use agent::now_milli;
use agent::tracked_task::TrackedTask;
use agent_common::agent_config::AgentConfig;

#[tokio::main]
async fn main() {
    let use_ui = enable_raw_mode().is_ok();

    let _guard = if use_ui {
        let file_appender = tracing_appender::rolling::daily("logs", "playit.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        tracing_subscriber::fmt().with_ansi(false).with_max_level(Level::INFO).with_writer(non_blocking).init();

        tracing::info!("staring with UI");
        Some(guard)
    } else {
        tracing_subscriber::fmt().with_ansi(false).with_max_level(Level::INFO).init();

        tracing::info!("staring without UI");
        None
    };

    let events = PlayitEvents::new();
    let agent_config = ManagedAgentConfig::new(events.clone());
    let render_state = Arc::new(RwLock::new(
        AgentState::PreparingConfig(agent_config.status.clone())
    ));

    let app = Application {
        events,
        agent_config,
        render_state,
    };

    let renderer = Renderer {
        render_count: 0,
        state: app.render_state.clone(),
    };

    let app_task = TrackedTask::new(app.start());

    if use_ui {
        let ui_task = start_terminal_ui(renderer, app_task);

        let app_task = match ui_task.await {
            Ok(Ok(_)) => {
                tracing::info!("program closed");
                return
            },
            Ok(Err(v)) => {
                tracing::warn!("got UI rendering error");
                v
            },
            Err(_) => return,
        };
        app_task.wait().await;
    } else {
        app_task.wait().await;
    }
}

async fn _get_initial_config(state: Arc<RwLock<AgentState>>) -> AgentConfig {
    let guard = state.read().await;
    let prepare_status = match &*guard {
        AgentState::PreparingConfig(status) => status,
        _ => panic!(),
    };
    let config = prepare_config(prepare_status).await.unwrap();

    /* wait 1s so user can read message */
    tokio::time::sleep(Duration::from_secs(1)).await;

    /* if we're showing a message wait an extra 5 seconds */
    match &*guard {
        AgentState::PreparingConfig(status) => {
            let status_guard = status.read().await;
            match &*status_guard {
                AgentConfigStatus::PleaseCreateAccount { .. } | AgentConfigStatus::PleaseVerifyAccount { .. } => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                _ => {}
            }
        }
        _ => panic!()
    }

    config
}

fn start_terminal_ui(mut renderer: Renderer, app_task: TrackedTask) -> JoinHandle<Result<TrackedTask, TrackedTask>> {
    tokio::task::spawn_blocking(move || {
        if enable_raw_mode().is_err() {
            return Err(app_task);
        }

        let mut stdout = std::io::stdout();
        if execute!(stdout, EnterAlternateScreen).is_err() {
            return Err(app_task);
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = match Terminal::new(backend) {
            Ok(v) => v,
            Err(_) => return Err(app_task),
        };

        let mut app_done_at = 0;

        loop {
            if app_task.is_done() {
                let now = now_milli();

                /* wait 20 seconds before closing application */
                if app_done_at == 0 {
                    app_done_at = now;
                } else if app_done_at + 20_000 < now {
                    break;
                }
            }

            if terminal.draw(|f| renderer.run(f)).is_err() {
                return Err(app_task);
            }

            let has_event = match event::poll(Duration::from_millis(300)) {
                Ok(v) => v,
                Err(_) => return Err(app_task),
            };

            if has_event {
                let event = match event::read() {
                    Ok(v) => v,
                    Err(_) => return Err(app_task),
                };

                if let Event::Key(key) = event {
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        break;
                    }
                }
            }
        }

        // restore terminal
        if disable_raw_mode().is_err() {
            return Err(app_task);
        }
        if execute!(terminal.backend_mut(), LeaveAlternateScreen).is_err() {
            return Err(app_task);
        }
        if terminal.show_cursor().is_err() {
            return Err(app_task);
        }

        Ok(app_task)
    })
}

pub struct Renderer {
    state: Arc<RwLock<AgentState>>,
    render_count: usize,
}

impl Renderer {
    pub fn run<B: Backend>(&mut self, f: &mut Frame<B>) {
        let size = f.size();
        self.render_count += 1;

        let title_bar = Gauge::default()
            .gauge_style(Style::default().fg(Color::Cyan))
            .label(Span::styled(format!("playit.gg v0.7.0 ({})", self.render_count), Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::UNDERLINED),
            ))
            .percent(100);
        f.render_widget(title_bar, Rect::new(0, 0, size.width, 1));

        {
            let guard = futures::executor::block_on(self.state.read());
            match &*guard {
                AgentState::PreparingConfig(status) => {
                    let status_guard = futures::executor::block_on(status.read());
                    self.render_preparing_config(f, &*status_guard);
                }
                AgentState::WaitingForTunnels { error } => {
                    self.render_no_tunnels(f, *error);
                }
                AgentState::Running(running) => {
                    self.render_running(f, running);
                }
                AgentState::ConnectingToTunnelServer => {
                    self.render_status_message(f, "connecting to tunnel server");
                }
                AgentState::FailedToConnect => {
                    self.render_status_message(f, "connecting to tunnel server");
                }
            }
        }
    }

    fn render_running<B: Backend>(&self, f: &mut Frame<B>, running: &RunningState) {
        let list_block = Block::default()
            .borders(Borders::ALL)
            .title("events")
            .border_type(BorderType::Thick);

        let events = running.events.with_events(|events| {
            let mut list_items = Vec::new();

            for i in (0..events.len()).rev() {
                let event = &events[i];
                let span = Span::from(format!("{} - {:?}", event.id, event.details));
                list_items.push(ListItem::new(span));
            }

            list_items
        });

        let list = List::new(events)
            .block(list_block)
            .start_corner(Corner::TopLeft);

        let size = f.size();
        f.render_widget(list, Rect::new(0, 1, size.width, size.height.max(1) - 1));
    }

    fn render_status_message<B: Backend>(&self, f: &mut Frame<B>, message: &str) {
        let description = Paragraph::new(message)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false });

        let size = f.size();
        let top_offset = ((size.height - 1) / 2).max(3) - 2;
        f.render_widget(description, Rect::new(0, top_offset, size.width, size.height - top_offset));
    }

    fn render_no_tunnels<B: Backend>(&self, f: &mut Frame<B>, error: bool) {
        let description = match error {
            true => Paragraph::new("No tunnels found, create them at\nhttps://new.playit.gg/account/tunnels\nGetting an error trying to load tunnels..."),
            false => Paragraph::new("No tunnels found, create them at\nhttps://new.playit.gg/account/tunnels"),
        }
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false });

        let size = f.size();
        let top_offset = ((size.height - 1) / 2).max(3) - 2;
        f.render_widget(description, Rect::new(0, top_offset, size.width, size.height - top_offset));
    }

    fn render_preparing_config<B: Backend>(&self, f: &mut Frame<B>, status: &AgentConfigStatus) {
        let description = match status {
            AgentConfigStatus::Staring => Paragraph::new("Starting program"),
            AgentConfigStatus::ReadingConfigFile => Paragraph::new("Reading config file"),
            AgentConfigStatus::PleaseActiveProgram { url } => Paragraph::new(
                format!("Setup required, please visit\n{}", url)
            ),
            AgentConfigStatus::PleaseVerifyAccount { url } => Paragraph::new(
                format!("Please verify your email\n{}", url)
            ),
            AgentConfigStatus::PleaseCreateAccount { url } => Paragraph::new(
                format!("Improve security, create an account\n{}", url)
            ),
            AgentConfigStatus::FileReadFailed => Paragraph::new("ERROR: Failed to read file"),
            AgentConfigStatus::LoadingAccountStatus => Paragraph::new("Loading account status"),
            AgentConfigStatus::ErrorLoadingAccountStatus => Paragraph::new("Failed to load account status"),
            AgentConfigStatus::AccountVerified => Paragraph::new("Found verified account"),
            AgentConfigStatus::ProgramActivated => Paragraph::new("Program activated"),
        }
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false });

        let size = f.size();

        let top_offset = ((size.height - 1) / 2).max(3) - 2;
        f.render_widget(description, Rect::new(0, top_offset, size.width, size.height - top_offset));
    }
}
