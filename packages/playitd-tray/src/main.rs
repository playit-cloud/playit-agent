#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(any(target_os = "windows", test))]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod presentation;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
fn main() {
    windows::main();
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("playitd-tray is only supported on Windows");
    std::process::exit(1);
}
