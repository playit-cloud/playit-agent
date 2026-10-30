#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
mod presentation;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
fn main() {
    windows::main();
}

#[cfg(target_os = "macos")]
fn main() {
    macos::main();
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn main() {
    eprintln!("playitd-tray is only supported on Windows and macOS");
    std::process::exit(1);
}
