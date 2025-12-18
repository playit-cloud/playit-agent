use std::io::stdout;

use crossterm::{
    cursor::RestorePosition,
    event::{self, Event, KeyCode, KeyEvent},
    style::{Print, ResetColor},
    terminal::Clear,
    ExecutableCommand,
};
use playit_agent_core::utils::now_milli;

use crate::signal_handle::get_signal_handle;
use crate::CliError;

pub struct UI {
    auto_answer: Option<bool>,
    last_display: Option<(u64, String)>,
    log_only: bool,
    wrote_content: bool,
}

#[derive(Default)]
pub struct UISettings {
    pub auto_answer: Option<bool>,
    pub log_only: bool,
}

impl UI {
    pub fn new(settings: UISettings) -> Self {
        UI {
            auto_answer: settings.auto_answer,
            log_only: settings.log_only,
            last_display: None,
            wrote_content: false,
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

        if self.log_only {
            return;
        }

        let content_ref = &content;
        let res: std::io::Result<()> = (|| {
            let cleared = if self.wrote_content {
                stdout()
                    .execute(Clear(crossterm::terminal::ClearType::All))
                    .is_ok()
            } else {
                true
            };

            if !cleared {
                stdout().execute(Print(format!("\n{}", content_ref)))?;
            } else {
                stdout()
                    .execute(RestorePosition)?
                    .execute(ResetColor)?
                    .execute(Print(content_ref))?;
            }

            Ok(())
        })();

        if let Err(error) = res {
            tracing::error!(?error, "failed to write to screen");
            println!("{}", content);
        }

        self.wrote_content = true;
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

    pub async fn write_error<M: std::fmt::Display, E: std::fmt::Debug>(
        &mut self,
        msg: M,
        error: E,
    ) {
        self.write_screen(format!("Got Error\nMSG: {}\nError: {:?}\n", msg, error))
            .await
    }
}
