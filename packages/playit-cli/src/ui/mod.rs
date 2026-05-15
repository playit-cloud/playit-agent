use std::io::{Write, stdin, stdout};

use playit_agent_core::utils::now_milli;

use crate::CliError;
use crate::signal_handle::get_signal_handle;

pub mod tui_app;
pub mod widgets;

pub use tui_app::{AgentData, ConnectionStats, TuiApp};

#[derive(Default, Clone)]
pub struct UISettings {
    pub auto_answer: Option<bool>,
}

pub struct ConsoleUi {
    auto_answer: Option<bool>,
    last_display: Option<(u64, String)>,
}

impl ConsoleUi {
    pub fn new(settings: UISettings) -> Self {
        Self {
            auto_answer: settings.auto_answer,
            last_display: None,
        }
    }

    pub async fn write_screen<T: std::fmt::Display>(&mut self, content: T) {
        let signal = get_signal_handle();
        if signal.is_confirming_close() {
            match self
                .yn_question(
                    format!("{content}\nClose requested, close program?"),
                    Some(true),
                )
                .await
            {
                Ok(true) => std::process::exit(0),
                Ok(false) => signal.decline_close(),
                Err(error) => {
                    eprintln!("failed to ask close signal question: {error}");
                }
            }
            return;
        }

        self.write_screen_inner(content.to_string());
    }

    fn write_screen_inner(&mut self, content: String) {
        if let Some((ts, last_render)) = &self.last_display {
            if now_milli() - *ts < 10_000 && content == *last_render {
                return;
            }
        }

        println!("{content}");
        self.last_display = Some((now_milli(), content));
    }

    pub async fn yn_question<T: std::fmt::Display + Send + 'static>(
        &mut self,
        question: T,
        default_yes: Option<bool>,
    ) -> Result<bool, CliError> {
        if let Some(auto) = self.auto_answer {
            return Ok(auto);
        }

        let prompt = question.to_string();
        tokio::task::spawn_blocking(move || -> Result<bool, CliError> {
            let prompt_suffix = match default_yes {
                Some(true) => "Y/n",
                Some(false) => "y/N",
                None => "y/n",
            };

            loop {
                print!("{prompt} ({prompt_suffix})? ");
                stdout().flush().map_err(CliError::RenderError)?;

                let mut line = String::new();
                stdin()
                    .read_line(&mut line)
                    .map_err(CliError::RenderError)?;
                let input = line.trim().to_lowercase();

                if input.is_empty() {
                    if let Some(default) = default_yes {
                        return Ok(default);
                    }
                }

                match input.as_str() {
                    "y" | "yes" => return Ok(true),
                    "n" | "no" => return Ok(false),
                    _ => println!("Please answer y or n."),
                }
            }
        })
        .await
        .map_err(|_| CliError::AnswerNotProvided)?
    }

    pub async fn write_error<M: std::fmt::Display, E: std::fmt::Debug>(
        &mut self,
        msg: M,
        error: E,
    ) {
        self.write_screen(format!("Got Error\nMSG: {msg}\nError: {error:?}\n"))
            .await;
    }
}
