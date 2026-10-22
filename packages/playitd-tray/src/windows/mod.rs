mod autostart;
mod backend;
mod backend_actions;
mod frontend;
mod protocol;
mod state;
mod util;

use std::ffi::OsString;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LaunchOptions {
    /// Started by the Run key at sign-in or by the installer; respects the user's
    /// "Launch at Login" choice and never shows startup error dialogs.
    pub(crate) autostart: bool,
    /// Started from the Start menu or desktop; also opens the playit window.
    pub(crate) open: bool,
}

pub(crate) fn main() {
    util::init_debug_console_from_args();

    let options = match parse_args(std::env::args_os().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            util::show_error("playit", &error);
            std::process::exit(2);
        }
    };

    if let Err(error) = frontend::run(options) {
        if options.autostart {
            util::debug_log(&format!("tray failed to start: {error}"));
        } else {
            util::show_error("Failed to start the playit tray", &error);
        }
        std::process::exit(1);
    }
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<LaunchOptions, String> {
    let mut options = LaunchOptions::default();
    for arg in args {
        match arg.to_string_lossy().as_ref() {
            "--autostart" => options.autostart = true,
            "--open" => options.open = true,
            "--debug-console" => {}
            other => return Err(format!("Unsupported playitd-tray argument: {other}")),
        }
    }
    Ok(options)
}
