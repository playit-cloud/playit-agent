mod backend;
mod backend_actions;
mod frontend;
mod protocol;
mod state;
mod util;

pub(crate) fn init_debug_console_from_args() {
    util::init_debug_console_from_args();
}

pub(crate) fn run() -> Result<(), String> {
    frontend::run()
}

pub(crate) fn show_error(title: &str, message: &str) {
    util::show_error(title, message);
}
