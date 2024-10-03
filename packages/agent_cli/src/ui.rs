use std::{borrow::Cow, fmt::{Debug, Display}, io::{stdout, Write}, process::ExitCode, time::{Duration, Instant}};

use crossterm::{
    cursor::RestorePosition,
    event::{self, Event, KeyCode, KeyEvent},
    ExecutableCommand,
    style::{Print, ResetColor},
    terminal::Clear,
};
use serde::Serialize;

use crate::{args::CliInterface, cli_io::{CliErrorPrint, CliMessage, CliUIMessage}, CliError};
use crate::signal_handle::get_signal_handle;

pub struct UI {
    auto_answer: Option<bool>,
    last_display: Option<(Instant, String)>,
    cli_interface: CliInterface,
}

#[derive(Default)]
pub struct UISettings {
    pub auto_answer: Option<bool>,
    pub log_only: bool,
    pub cli_interface: CliInterface,
}

pub trait UIMessage: Display + Serialize {
    fn is_fullscreen(&self) -> bool;
    
    fn write_human(&self) -> String {
        self.to_string()
    }

    fn write_json(&self) -> Option<String> {
        Some(serde_json::to_string(self).unwrap())
    }

    fn write_csv(&self) -> Option<String> {
        None
    }
}

impl UIMessage for String {
    fn is_fullscreen(&self) -> bool {
        false
    }
}

impl<'a> UIMessage for &'a str {
    fn is_fullscreen(&self) -> bool {
        false
    }
}

impl<'a> UIMessage for &'a String {
    fn is_fullscreen(&self) -> bool {
        false
    }
}

impl UI {
    pub fn new(settings: UISettings) -> Self {
        UI {
            auto_answer: settings.auto_answer,
            last_display: None,
            cli_interface: settings.cli_interface,
        }
    }

    fn build_string<T: UIMessage>(&self, value: &T) -> Option<String> {
        match self.cli_interface {
            CliInterface::Human => Some(value.write_human()),
            CliInterface::Csv => value.write_csv(),
            CliInterface::Json => value.write_json(),
        }
    }

    pub async fn write_status<S: Into<StatusMessage>>(&mut self, status: S) {
        self.write_screen(status.into()).await;
    }

    pub async fn write_message<T: CliMessage>(&mut self, msg: T) {
        self.write_screen(CliUIMessage::from(msg)).await
    }

    pub async fn write_screen<T: UIMessage>(&mut self, content: T) {
        let signal = get_signal_handle();
        let exit_confirm = signal.is_confirming_close();

        if exit_confirm {
            /* only confirm exit with human interface */
            if self.cli_interface != CliInterface::Human {
                tracing::info!("received Ctrl+C, closing program");
                std::process::exit(0);
            }

            match self.yn_question(format!("{}\nClose requested, close program?", content), Some(true)).await {
                Ok(close) => {
                    if close {
                        std::process::exit(0);
                    } else {
                        signal.decline_close();
                    }
                },
                Err(error) => {
                    tracing::error!(%error, "failed to ask close signal question, closing program");
                    std::process::exit(0);
                }
            }
        }

        self.write_screen_inner(content)
    }

    fn write_screen_inner<T: UIMessage>(&mut self, content: T) {
        let Some(content_str) = self.build_string(&content) else { return };

        {
            if let Some((ts, last_render)) = &self.last_display {
                if ts.elapsed() < Duration::from_secs(3) && content_str.eq(last_render) {
                    return;
                }
            }

            self.last_display = Some((Instant::now(), content_str.clone()));
        }

        if !content.is_fullscreen() || self.cli_interface != CliInterface::Human {
            println!("{}", content_str);
            return;
        }

        let content_ref = &content_str;
        let res: std::io::Result<()> = (|| {
            let cleared = stdout()
                    .execute(Clear(crossterm::terminal::ClearType::All))
                    .is_ok();

            if !cleared {
                stdout()
                    .execute(Print(format!("\n{}\n", content_ref)))?;
            } else {
                stdout()
                    .execute(RestorePosition)?
                    .execute(ResetColor)?
                    .execute(Print(format!("{}\n", content_ref)))?;
            }

            Ok(())
        })();

        if let Err(error) = res {
            tracing::error!(?error, "failed to write to screen");
            println!("{}", content_str);
        }
    }

    pub async fn yn_question<T: std::fmt::Display + Send + 'static>(&mut self, question: T, default_yes: Option<bool>) -> Result<bool, CliError> {
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
                    self.write_screen_inner(format!("{}{} (Y/n)? ", pref, question));
                } else {
                    self.write_screen_inner(format!("{}{} (y/N)? ", pref, question));
                }
            } else {
                self.write_screen_inner(format!("{}{} (y/n)? ", pref, question));
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

    pub fn ok_or_fatal<O, E: Debug + Serialize>(&mut self, result: Result<O, E>, msg: &str) -> O {
        match result {
            Ok(v) => v,
            Err(error) => {
                tracing::error!(msg, "fatal error");
                self.write_error(msg, error);
                std::process::exit(1);
            }
        }
    }

    pub fn write_error<A: ToString, E: Debug + Serialize>(&mut self, message: A, error: E) {
        self.write_error2(CliErrorPrint { message: message.to_string(), error });
    }

    pub fn write_error2<E: Debug + Serialize>(&mut self, error: CliErrorPrint<E>) {
        tracing::error!(error = ?error.error, "{}", error.message);
        self.write_screen_inner(CliUIMessage::from(error));
    }
}


#[derive(Serialize)]
pub struct StatusMessage(pub Cow<'static, str>);

impl Into<StatusMessage> for &'static str {
    fn into(self) -> StatusMessage {
        StatusMessage(Cow::Borrowed(self))
    }
}

impl<'a> Into<StatusMessage> for &'a String {
    fn into(self) -> StatusMessage {
        StatusMessage(Cow::Owned(self.clone()))
    }
}

impl Into<StatusMessage> for String {
    fn into(self) -> StatusMessage {
        StatusMessage(Cow::Owned(self))
    }
}

impl Display for StatusMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Status: {}", self.0)
    }
}

impl UIMessage for StatusMessage {
    fn is_fullscreen(&self) -> bool {
        true
    }

    fn write_json(&self) -> Option<String> {
        None
    }

    fn write_csv(&self) -> Option<String> {
        None
    }
}
