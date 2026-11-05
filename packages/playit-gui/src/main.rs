// Keep Windows release builds from opening a console next to the window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;
use gpui::{
    App, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, actions,
    prelude::*, px, size,
};
use playit_ipc::ipc::get_default_socket_path;
use tracing_subscriber::EnvFilter;

use crate::assets::Assets;
use crate::state::{AppState, SessionMode};
use crate::view::PlayitWindow;

mod assets;
mod backend;
mod state;
mod view;

actions!(playit, [Quit]);

#[derive(Parser)]
#[command(name = "playit-gui", version)]
struct Cli {
    /// Override the IPC socket or named pipe used to reach playitd
    #[arg(long)]
    socket_path: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("PLAYIT_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let session_mode = match cli.socket_path {
        Some(_) => SessionMode::ExternalDaemon,
        None => SessionMode::Service,
    };
    let socket_path = cli
        .socket_path
        .unwrap_or_else(|| get_default_socket_path().to_string());

    let runtime = tokio::runtime::Runtime::new().expect("failed to start the tokio runtime");
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (outcome_tx, outcome_rx) = tokio::sync::mpsc::unbounded_channel();
    backend::spawn(
        runtime.handle(),
        socket_path.clone(),
        event_tx,
        command_rx,
        outcome_tx,
    );

    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("ctrl-q", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(1040.), px(860.)), cx);
            let state = AppState::new(socket_path, session_mode);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(640.), px(480.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("playit".into()),
                        ..Default::default()
                    }),
                    app_id: Some("gg.playit.gui".to_string()),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| PlayitWindow::new(state, command_tx, event_rx, outcome_rx, cx)),
            )
            .expect("failed to open the playit window");
            cx.activate(true);
        });

    drop(runtime);
}
