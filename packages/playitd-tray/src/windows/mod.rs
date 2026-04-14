mod actions;
mod app;
mod runtime;
mod state;
mod util;

pub(crate) fn init_debug_console_from_args() {
    util::init_debug_console_from_args();
}

pub(crate) fn run() -> Result<(), String> {
    app::run()
}

pub(crate) fn show_error(title: &str, message: &str) {
    util::show_error(title, message);
}
