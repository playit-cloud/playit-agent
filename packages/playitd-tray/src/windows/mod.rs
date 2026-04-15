mod backend;
mod backend_actions;
mod frontend;
mod protocol;
mod state;
mod util;

use std::ffi::OsStr;

pub(crate) fn init_debug_console_from_args() {
    util::init_debug_console_from_args();
}

enum StartupShortcutMode {
    RunTray,
    EnsureShortcut,
    RemoveShortcut,
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

    fn silent(message: String) -> Self {
        Self {
            message,
            show_dialog: false,
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
    match startup_shortcut_mode().map_err(RunError::interactive)? {
        StartupShortcutMode::RunTray => frontend::run().map_err(RunError::interactive),
        StartupShortcutMode::EnsureShortcut => {
            backend_actions::ensure_startup_shortcut().map_err(RunError::silent)
        }
        StartupShortcutMode::RemoveShortcut => {
            backend_actions::remove_startup_shortcut().map_err(RunError::silent)
        }
    }
}

pub(crate) fn show_error(title: &str, message: &str) {
    util::show_error(title, message);
}

fn startup_shortcut_mode() -> Result<StartupShortcutMode, String> {
    let mut mode = StartupShortcutMode::RunTray;

    for arg in std::env::args_os().skip(1) {
        if arg == OsStr::new("--debug-console") {
            continue;
        }

        match arg.as_os_str() {
            value if value == OsStr::new("--ensure-startup-shortcut") => {
                if matches!(mode, StartupShortcutMode::RemoveShortcut) {
                    return Err(
                        "Cannot combine --ensure-startup-shortcut with --remove-startup-shortcut"
                            .to_string(),
                    );
                }
                mode = StartupShortcutMode::EnsureShortcut;
            }
            value if value == OsStr::new("--remove-startup-shortcut") => {
                if matches!(mode, StartupShortcutMode::EnsureShortcut) {
                    return Err(
                        "Cannot combine --remove-startup-shortcut with --ensure-startup-shortcut"
                            .to_string(),
                    );
                }
                mode = StartupShortcutMode::RemoveShortcut;
            }
            _ => {
                return Err(format!(
                    "Unsupported playitd-tray argument: {}",
                    arg.to_string_lossy()
                ));
            }
        }
    }

    Ok(mode)
}
