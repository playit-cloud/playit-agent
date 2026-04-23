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

#[derive(Debug)]
enum StartupShortcutMode {
    RunTray,
    EnsureShortcut,
    RemoveShortcut,
    WriteInstalledUserSid,
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
        StartupShortcutMode::WriteInstalledUserSid => {
            backend_actions::write_installed_user_sid().map_err(RunError::silent)
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
                if !matches!(mode, StartupShortcutMode::RunTray) {
                    return Err(
                        "Cannot combine --ensure-startup-shortcut with another helper mode"
                            .to_string(),
                    );
                }
                mode = StartupShortcutMode::EnsureShortcut;
            }
            value if value == OsStr::new("--remove-startup-shortcut") => {
                if !matches!(mode, StartupShortcutMode::RunTray) {
                    return Err(
                        "Cannot combine --remove-startup-shortcut with another helper mode"
                            .to_string(),
                    );
                }
                mode = StartupShortcutMode::RemoveShortcut;
            }
            value if value == OsStr::new("--write-installed-user-sid") => {
                if !matches!(mode, StartupShortcutMode::RunTray) {
                    return Err(
                        "Cannot combine --write-installed-user-sid with another helper mode"
                            .to_string(),
                    );
                }
                mode = StartupShortcutMode::WriteInstalledUserSid;
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

#[cfg(test)]
mod tests {
    use super::StartupShortcutMode;

    fn parse_mode(args: &[&str]) -> Result<StartupShortcutMode, String> {
        let mut mode = StartupShortcutMode::RunTray;

        for arg in args {
            match *arg {
                "--debug-console" => {}
                "--ensure-startup-shortcut" => {
                    if !matches!(mode, StartupShortcutMode::RunTray) {
                        return Err("duplicate helper mode".to_string());
                    }
                    mode = StartupShortcutMode::EnsureShortcut;
                }
                "--remove-startup-shortcut" => {
                    if !matches!(mode, StartupShortcutMode::RunTray) {
                        return Err("duplicate helper mode".to_string());
                    }
                    mode = StartupShortcutMode::RemoveShortcut;
                }
                "--write-installed-user-sid" => {
                    if !matches!(mode, StartupShortcutMode::RunTray) {
                        return Err("duplicate helper mode".to_string());
                    }
                    mode = StartupShortcutMode::WriteInstalledUserSid;
                }
                _ => return Err("unsupported argument".to_string()),
            }
        }

        Ok(mode)
    }

    #[test]
    fn parses_write_installed_user_sid_mode() {
        let mode = parse_mode(&["--write-installed-user-sid"]).unwrap();
        assert!(matches!(mode, StartupShortcutMode::WriteInstalledUserSid));
    }

    #[test]
    fn write_installed_user_sid_cannot_be_combined_with_other_helper_modes() {
        let error = parse_mode(&["--write-installed-user-sid", "--ensure-startup-shortcut"])
            .expect_err("combined helper modes should fail");
        assert_eq!(error, "duplicate helper mode");
    }
}
