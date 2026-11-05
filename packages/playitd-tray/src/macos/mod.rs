//! The playit menu bar app. It shows whether the agent is online and manages the per-user
//! LaunchAgent that runs playitd. It ships as a helper app inside playit.app, whose main
//! executable is the playit window; the playit and playitd binaries sit beside that.

mod actions;
mod app;
mod login_item;
mod util;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LaunchOptions {
    /// Started by the login item; respects "Open at Login" and never shows dialogs.
    pub(crate) autostart: bool,
    /// Started by the installer; opens playit only when the agent still needs setup.
    pub(crate) after_install: bool,
    /// Started by the playit window, which is already open.
    pub(crate) background: bool,
}

impl LaunchOptions {
    /// Launched directly by the user, who expects the playit window to open.
    pub(crate) fn opens_playit(&self) -> bool {
        !self.autostart && !self.after_install && !self.background
    }
}

pub(crate) fn main() {
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            util::show_error("playit", &error);
            std::process::exit(2);
        }
    };

    if let Err(error) = app::run(options) {
        if options.autostart {
            util::log(&format!("menu bar app failed to start: {error}"));
        } else {
            util::show_error("Failed to start playit", &error);
        }
        std::process::exit(1);
    }
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<LaunchOptions, String> {
    let mut options = LaunchOptions::default();
    for arg in args {
        match arg.as_str() {
            "--autostart" => options.autostart = true,
            "--after-install" => options.after_install = true,
            "--background" => options.background = true,
            // Older macOS versions pass a process serial number when Finder opens the app.
            other if other.starts_with("-psn_") => {}
            other => return Err(format!("Unsupported playitd-tray argument: {other}")),
        }
    }
    Ok(options)
}
