use std::path::PathBuf;

use clap::Parser;
use playitd::{DaemonOptions, VersionDetails, default_secret_path, load_version_overrides};

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
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    let mut version = match VersionDetails::from_cargo_package() {
        Ok(version) => version,
        Err(error) => {
            eprintln!("playitd version error: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    if let Some(path) = cli.version_overrides.as_deref() {
        match load_version_overrides(path).await {
            Ok(overrides) => version.apply_overrides(overrides),
            Err(error) => {
                eprintln!("playitd version override error: {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

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
    };

    match playitd::run_daemon(options).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("playitd error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
