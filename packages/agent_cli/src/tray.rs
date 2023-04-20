use std::ptr;
use tray_item::TrayItem;
use winapi::um::wincon::GetConsoleWindow;
use winapi::um::winuser::{ShowWindow, SW_HIDE, SW_SHOW};
use anyhow;

enum TrayEvent {
    Hide,
    Show,
    Quit,
}

pub async fn setup_tray() -> Result<(), anyhow::Error> {
    let (tx, rx) = std::sync::mpsc::channel::<TrayEvent>();

    let mut tray = TrayItem::new("Playit", "APPICON")?;

    tray.add_label("Playit")?;

    let tx_hide = tx.clone();
    tray.add_menu_item("Hide", move || {
        tx_hide.send(TrayEvent::Hide).unwrap();
    })?;

    let tx_show = tx.clone();
    tray.add_menu_item("Show", move || {
        tx_show.send(TrayEvent::Show).unwrap();
    })?;

    let tx_quit = tx.clone();
    tray.add_menu_item("Quit", move || {
        tx_quit.send(TrayEvent::Quit).unwrap();
    })?;

    loop {
        match rx.recv() {
            Ok(TrayEvent::Hide) => {
                hide_console_window()?;
            }
            Ok(TrayEvent::Show) => {
                show_console_window()?;
            }
            Ok(TrayEvent::Quit) => {
                std::process::exit(0);
            }
            Err(_) => {}
        }
    }
}
// windows only
fn hide_console_window() -> Result<(), anyhow::Error> {
    let window = unsafe { GetConsoleWindow() };
    if window != ptr::null_mut() {
        unsafe { ShowWindow(window, SW_HIDE) };
    }
    Ok(())
}

fn show_console_window() -> Result<(), anyhow::Error> {
    let window = unsafe { GetConsoleWindow() };
    if window != ptr::null_mut() {
        unsafe { ShowWindow(window, SW_SHOW) };
    }
    Ok(())
}