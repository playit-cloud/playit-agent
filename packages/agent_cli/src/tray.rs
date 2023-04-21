#![cfg(target_os = "windows")]

use std::ptr;
use tray_item::TrayItem;
use winapi::um::wincon::{GetConsoleWindow, CTRL_C_EVENT, CTRL_CLOSE_EVENT};
use winapi::um::winuser::{ShowWindow, SW_HIDE, SW_SHOW};
use anyhow;

enum TrayEvent {
    Hide,
    Show,
    Quit,
}

static mut GLOBAL_TRAY_ITEM: Option<TrayItem> = None;

pub async fn setup_tray() -> Result<(), anyhow::Error> {
    let mut tray = TrayItem::new(format!("Playit-{}", env!("CARGO_PKG_VERSION")).as_str(), "APPICON")?;

    unsafe {
        winapi::um::consoleapi::SetConsoleCtrlHandler(Some(ctrl_handler), 1);
    }

    let (tx, rx) = std::sync::mpsc::channel::<TrayEvent>();
    tray.add_label(format!("Playit {}", env!("CARGO_PKG_VERSION")).as_str())?;

    let tx_hide = tx.clone();
    tray.add_menu_item("hide", move || {
        tx_hide.send(TrayEvent::Hide).unwrap();
    })?;

    let tx_show = tx.clone();
    tray.add_menu_item("show", move || {
        tx_show.send(TrayEvent::Show).unwrap();
    })?;

    let tx_quit = tx.clone();
    tray.add_menu_item("quit", move || {
        tx_quit.send(TrayEvent::Quit).unwrap();
    })?;

    unsafe { GLOBAL_TRAY_ITEM = Some(tray) };

    loop {
        match rx.recv() {
            Ok(TrayEvent::Hide) => {
                hide_console_window()?;
            }
            Ok(TrayEvent::Show) => {
                show_console_window()?;
            }
            Ok(TrayEvent::Quit) => {
                unsafe {
                    if let Some(tray) = &mut GLOBAL_TRAY_ITEM {
                        tray.inner_mut().shutdown().ok();
                        tray.inner_mut().quit();
                    }
                }
                std::process::exit(0);
            }
            Err(_) => {}
        }
    }
}

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

// the tray needs to be deconstruced before ending the program, otherwise it can cause "ghost tray icons"
// mentioned here: https://github.com/olback/tray-item-rs/issues/13
// this works for closing the window (x button) and ctrl+c

extern "system" fn ctrl_handler(ctrl_type: u32) -> i32 {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_CLOSE_EVENT  => {
            unsafe {
                if let Some(tray) = &mut GLOBAL_TRAY_ITEM {
                    tray.inner_mut().shutdown().ok();
                    tray.inner_mut().quit();
                
                }
            }
            std::process::exit(0);
        }
        _ => 1,
    }
}