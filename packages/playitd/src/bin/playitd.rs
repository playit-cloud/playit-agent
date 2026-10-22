#![cfg_attr(target_os = "windows", windows_subsystem = "console")]

use std::path::PathBuf;

use clap::Parser;
use playitd::{DaemonOptions, VersionDetails, default_secret_path, load_version_overrides};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "playitd")]
struct Cli {
    /// Inline secret key for the daemon
    #[arg(long, conflicts_with = "secret_path")]
    secret: Option<String>,

    /// Path to the daemon secret file
    #[arg(long, conflicts_with = "secret")]
    secret_path: Option<PathBuf>,

    /// Override the IPC socket or named pipe path
    #[arg(long)]
    socket_path: Option<String>,

    /// Path to write daemon logs to
    #[arg(short = 'l', long)]
    log_path: Option<PathBuf>,

    /// Overrides platform registration to be docker
    #[arg(long)]
    platform_docker: bool,

    /// JSON or YAML file containing version overrides
    #[arg(long)]
    version_overrides: Option<PathBuf>,

    /// Run as the installed Windows service (used by the service control manager)
    #[cfg(target_os = "windows")]
    #[arg(long, hide = true)]
    windows_service: bool,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("playitd failed to start tokio: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let version = match runtime.block_on(resolve_version(&cli)) {
        Ok(version) => version,
        Err(error) => {
            eprintln!("playitd version error: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    #[cfg(target_os = "windows")]
    if cli.windows_service {
        // The service host builds its own runtime on the service thread.
        drop(runtime);
        let options = playitd::windows::service_host::service_options(version);
        return match playitd::windows::service_host::run(options) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    let shutdown = CancellationToken::new();
    let options = DaemonOptions {
        secret: cli.secret.clone(),
        secret_path: if cli.secret.is_some() {
            None
        } else {
            Some(cli.secret_path.unwrap_or_else(default_secret_path))
        },
        socket_path: cli.socket_path,
        log_path: cli.log_path,
        platform_docker: cli.platform_docker,
        version,
        shutdown: Some(shutdown.clone()),
    };

    runtime.block_on(async move {
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("Received Ctrl+C, shutting down");
                shutdown.cancel();
            }
        });

        match playitd::run_daemon(options).await {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("playitd error: {error}");
                std::process::ExitCode::FAILURE
            }
        }
    })
}

async fn resolve_version(cli: &Cli) -> Result<VersionDetails, String> {
    let mut version = VersionDetails::from_cargo_package()?;
    if let Some(path) = cli.version_overrides.as_deref() {
        version.apply_overrides(load_version_overrides(path).await?);
    }
    Ok(version)
}
