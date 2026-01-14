use std::sync::Arc;

use crossterm::event::{self, Event, KeyCode, KeyEvent};
use playit_agent_core::utils::now_milli;

use crate::signal_handle::get_signal_handle;
use crate::CliError;

pub mod log_capture;
pub mod tui_app;
pub mod widgets;

pub use log_capture::LogCapture;
pub use tui_app::{AgentData, TuiApp};

/// UI mode - either TUI (interactive) or log-only (stdout)
pub enum UI {
    Tui(Box<TuiApp>),
    LogOnly(LogOnlyUI),
}

#[derive(Default, Clone)]
pub struct UISettings {
    pub auto_answer: Option<bool>,
    pub log_only: bool,
}

impl UI {
    pub fn new(settings: UISettings) -> Self {
        if settings.log_only {
            UI::LogOnly(LogOnlyUI::new(settings))
        } else {
            UI::Tui(Box::new(TuiApp::new(settings)))
        }
    }

    pub async fn write_screen<T: std::fmt::Display>(&mut self, content: T) {
        match self {
            UI::Tui(tui) => tui.write_screen(content).await,
            UI::LogOnly(log_only) => log_only.write_screen(content).await,
        }
    }

    pub async fn yn_question<T: std::fmt::Display + Send + 'static>(
        &mut self,
        question: T,
        default_yes: Option<bool>,
    ) -> Result<bool, CliError> {
        match self {
            UI::Tui(tui) => tui.yn_question(question, default_yes).await,
            UI::LogOnly(log_only) => log_only.yn_question(question, default_yes).await,
        }
    }

    pub async fn write_error<M: std::fmt::Display, E: std::fmt::Debug>(
        &mut self,
        msg: M,
        error: E,
    ) {
        self.write_screen(format!("Got Error\nMSG: {}\nError: {:?}\n", msg, error))
            .await
    }

    /// Update UI with agent data (for TUI mode)
    pub fn update_agent_data(&mut self, data: AgentData) {
        if let UI::Tui(tui) = self {
            tui.update_agent_data(data);
        }
    }

    /// Get the log capture for TUI mode
    pub fn log_capture(&self) -> Option<Arc<LogCapture>> {
        if let UI::Tui(tui) = self {
            Some(tui.log_capture())
        } else {
            None
        }
    }

    /// Run one iteration of the TUI event loop
    /// Returns Ok(true) if should continue, Ok(false) if should quit
    pub fn tick_tui(&mut self) -> Result<bool, CliError> {
        if let UI::Tui(tui) = self {
            tui.tick()
        } else {
            Ok(true)
        }
    }

    /// Shutdown the TUI
    pub fn shutdown_tui(&mut self) -> Result<(), CliError> {
        if let UI::Tui(tui) = self {
            tui.shutdown()
        } else {
            Ok(())
        }
    }

    /// Check if TUI mode is active
    pub fn is_tui(&self) -> bool {
        matches!(self, UI::Tui(_))
    }
}

/// Log-only UI mode (original behavior)
pub struct LogOnlyUI {
    auto_answer: Option<bool>,
    last_display: Option<(u64, String)>,
}

impl LogOnlyUI {
    pub fn new(settings: UISettings) -> Self {
        LogOnlyUI {
            auto_answer: settings.auto_answer,
            last_display: None,
        }
    }

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

        self.write_screen_inner(content).await
    }

    async fn write_screen_inner<T: std::fmt::Display>(&mut self, content: T) {
        {
            let content = content.to_string();

            if let Some((ts, last_render)) = &self.last_display {
                if now_milli() - *ts < 10_000 && content.eq(last_render) {
                    return;
                }
            }

            tracing::info!("{}", content.lines().next().unwrap());
            self.last_display = Some((now_milli(), content));
        }
    }

    pub async fn yn_question<T: std::fmt::Display + Send + 'static>(
        &mut self,
        question: T,
        default_yes: Option<bool>,
    ) -> Result<bool, CliError> {
        let mut line = String::new();
        let mut count = 0;

        'ask_loop: loop {
            count += 1;

            let pref = if count == 1 {
                "".to_string()
            } else {
                format!("Invalid input: {:?}\n", line)
            };

            line.clear();

            if let Some(default_yes) = default_yes {
                if default_yes {
                    self.write_screen_inner(format!("{}{} (Y/n)? ", pref, question))
                        .await;
                } else {
                    self.write_screen_inner(format!("{}{} (y/N)? ", pref, question))
                        .await;
                }
            } else {
                self.write_screen_inner(format!("{}{} (y/n)? ", pref, question))
                    .await;
            }

            loop {
                let code = match tokio::task::spawn_blocking(|| event::read()).await.unwrap() {
                    Ok(Event::Key(KeyEvent { code, .. })) => code,
                    _ => break 'ask_loop,
                };

                match code {
                    KeyCode::Enter => {
                        let input = line.trim().to_lowercase();
                        if input.len() == 0 {
                            if let Some(default_yes) = default_yes {
                                return Ok(default_yes);
                            }
                        }

                        if input.eq("y") || input.eq("yes") {
                            return Ok(true);
                        }

                        if input.eq("n") || input.eq("no") {
                            return Ok(false);
                        }

                        break;
                    }
                    KeyCode::Char(c) => {
                        line.push(c);
                    }
                    _ => {}
                }
            }
        }

        if let Some(auto) = self.auto_answer {
            return Ok(auto);
        }

        if let Some(default_yes) = default_yes {
            return Ok(default_yes);
        }

        Err(CliError::AnswerNotProvided)
    }
}
