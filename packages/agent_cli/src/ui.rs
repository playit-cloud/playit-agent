use std::io::stdout;

use crossterm::{
    cursor::RestorePosition,
    event::{self, Event, KeyCode, KeyEvent},
    ExecutableCommand,
    style::{Print, ResetColor},
    terminal::Clear,
};

use crate::CliError;

pub struct UI {
    pub auto_answer: Option<bool>,
}

impl UI {
    pub fn write_screen<T: std::fmt::Display>(&mut self, content: T) -> Result<(), CliError> {
        let res: std::io::Result<()> = (|| {
            stdout()
                .execute(RestorePosition)?
                .execute(ResetColor)?
                .execute(Clear(crossterm::terminal::ClearType::All))?
                .execute(Print(content))?;

            Ok(())
        })();

        res.map_err(|e| CliError::RenderError(e))
    }

    pub fn yn_question<T: std::fmt::Display>(&mut self, question: T, default_yes: Option<bool>) -> Result<bool, CliError> {
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
                    self.write_screen(format!("{}{} (Y/n)? ", pref, question))?;
                } else {
                    self.write_screen(format!("{}{} (y/N)? ", pref, question))?;
                }
            } else {
                self.write_screen(format!("{}{} (y/n)? ", pref, question))?;
            }

            loop {
                let code = match event::read() {
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

    pub fn write_error<M: std::fmt::Display, E: std::fmt::Debug>(
        &mut self,
        msg: M,
        error: E,
    ) -> Result<(), CliError> {
        self.write_screen(format!("Got Error\nMSG: {}\nError: {:?}\n", msg, error))
    }
}
