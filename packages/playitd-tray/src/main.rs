#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
fn main() {
    windows::init_debug_console_from_args();
    if let Err(error) = windows::run() {
        windows::show_error("Failed to start playit tray", &error);
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("playitd-tray is only supported on Windows");
    std::process::exit(1);
}
