use std::path::PathBuf;

use clap::Parser;
use playitd::{DaemonOptions, VersionDetails, default_config_path, load_version_overrides};

#[derive(Parser)]
#[command(name = "playitd")]
struct Cli {
    /// Path to the daemon config file containing the secret key
    #[arg(long)]
    config_path: Option<PathBuf>,

    /// Override the IPC socket or named pipe path
    #[arg(long)]
    socket_path: Option<String>,

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
        config_path: cli.config_path.unwrap_or_else(default_config_path),
        socket_path: cli.socket_path,
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
