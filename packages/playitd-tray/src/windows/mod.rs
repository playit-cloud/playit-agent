mod backend;
mod backend_actions;
mod frontend;
mod protocol;
mod startup_shortcut;
mod state;
mod util;

use std::ffi::{OsStr, OsString};

pub(crate) fn init_debug_console_from_args() {
    util::init_debug_console_from_args();
}

pub(crate) struct RunError {
    message: String,
    show_dialog: bool,
}

impl RunError {
    fn interactive(message: String) -> Self {
        Self {
            message,
            show_dialog: true,
        }
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn should_show_dialog(&self) -> bool {
        self.show_dialog
    }
}

pub(crate) fn run_from_args() -> Result<(), RunError> {
    validate_args().map_err(RunError::interactive)?;
    frontend::run().map_err(RunError::interactive)
}

pub(crate) fn show_error(title: &str, message: &str) {
    util::show_error(title, message);
}

fn validate_args() -> Result<(), String> {
    validate_arg_values(std::env::args_os().skip(1))
}

fn validate_arg_values(args: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    for arg in args {
        if arg == OsStr::new("--debug-console") {
            continue;
        }

        return Err(format!(
            "Unsupported playitd-tray argument: {}",
            arg.to_string_lossy()
        ));
    }

    Ok(())
}
