use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;
use std::time::Duration;

use clap::{Parser, Subcommand};
use client::{
    AttachMode, CliTarget, begin_service_claim, complete_service_claim,
    run_account_login_url_command, run_attach_command, run_auto_command, run_reset_command,
    run_secret_path_command, run_start_command, run_status_command, run_stop_command,
};
use playit_agent_core::agent_control::platform::current_platform;
use playit_agent_core::agent_control::version::{help_register_version, register_platform};
use service::ServiceManagerMode;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use playit_agent_core::agent_control::errors::SetupError;
use playit_agent_core::gateway::Platform as RuntimePlatform;
use playit_agent_core::utils::now_milli;
use playit_runtime::{ClaimExchange, ClaimMode, ClaimProgress, ClaimService};

use crate::signal_handle::get_signal_handle;
use crate::ui::{ConsoleUi, UISettings};

pub static API_BASE: LazyLock<String> =
    LazyLock::new(|| dotenv::var("API_BASE").unwrap_or("https://api.playit.gg".to_string()));

mod client;
#[cfg(target_os = "linux")]
mod linux;
mod problem;
mod service;
pub mod signal_handle;
pub mod ui;
pub mod util;

#[derive(Parser)]
#[command(name = "playit-cli")]
struct Cli {
    /// Prints logs to stdout
    #[arg(short = 's', long)]
    stdout: bool,

    /// Override the IPC socket or named pipe used to reach playitd
    #[arg(long)]
    socket_path: Option<String>,

    #[cfg(target_os = "linux")]
    #[arg(long, conflicts_with = "openrc")]
    systemd: bool,

    #[cfg(target_os = "linux")]
    #[arg(long, conflicts_with = "systemd")]
    openrc: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Print version information
    Version,

    /// Attach to a running playitd service
    Attach {
        /// Print logs to stdout instead of using TUI
        #[arg(short = 's', long)]
        stdout: bool,
    },

    /// Start the installed playitd service
    Start,

    /// Stop the installed playitd service
    Stop,

    /// Show the status of the installed playitd service
    Status,

    /// Removes the secret key on your system so the playit agent can be re-claimed
    Reset,

    /// Shows the file path where the playit secret can be found
    SecretPath,

    /// Setup playit by provisioning a new secret to playitd
    Setup,

    /// Account management commands
    Account {
        #[command(subcommand)]
        command: AccountCommands,
    },

    /// Setting up a new playit agent
    #[command(
        about = "Setting up a new playit agent",
        long_about = "Provides a URL that can be visited to claim the agent and generate a secret key"
    )]
    Claim {
        #[command(subcommand)]
        command: ClaimCommands,
    },
}

#[derive(Subcommand)]
enum AccountCommands {
    /// Generates a link to allow user to login
    LoginUrl,
}

#[derive(Subcommand)]
enum ClaimCommands {
    /// Generates a random claim code
    Generate,

    /// Print a claim URL given the code and options
    Url {
        /// Claim code
        claim_code: String,

        /// Name for the agent
        #[arg(long, default_value = "from-cli")]
        name: String,

        /// The agent type
        #[arg(long, default_value = "self-managed")]
        r#type: String,
    },

    /// Exchanges the claim for the secret key
    Exchange {
        /// Claim code (see "claim generate")
        claim_code: String,

        /// Number of seconds to wait (0=infinite)
        #[arg(long, default_value = "0")]
        wait: u32,
    },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let signal_task = tokio::spawn(get_signal_handle().watch());
    let result = run_cli().await;
    signal_task.abort();
    let _ = signal_task.await;

    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run_cli() -> Result<std::process::ExitCode, CliError> {
    let cli = Cli::parse();

    /* register docker */
    {
        let platform = current_platform();

        register_platform(platform);

        help_register_version(
            env!("CARGO_PKG_VERSION"),
            "308943e8-faef-4835-a2ba-270351f72aa3",
        );
    }

    let target = CliTarget::from_socket_path(cli.socket_path.clone());
    let service_manager = service_manager_mode(&cli);
    let attach_stdout = matches!(&cli.command, Some(Commands::Attach { stdout: true, .. }));
    let stdout_mode = cli.stdout || attach_stdout;
    let attach_mode = if stdout_mode {
        AttachMode::Stdout
    } else {
        AttachMode::Interactive
    };

    let _guard = if stdout_mode {
        Some(init_stdout_tracing())
    } else {
        None
    };

    let mut console = ConsoleUi::new(UISettings { auto_answer: None });

    match cli.command {
        None => {
            run_auto_command(&mut console, &target, attach_mode, service_manager).await?;
        }
        Some(Commands::Attach { stdout }) => {
            let attach_mode = if cli.stdout || stdout {
                AttachMode::Stdout
            } else {
                AttachMode::Interactive
            };
            run_attach_command(&target, attach_mode).await?;
        }
        Some(Commands::Start) => {
            run_start_command(&mut console, &target, service_manager).await?;
        }
        Some(Commands::Stop) => {
            run_stop_command(&target, service_manager).await?;
        }
        Some(Commands::Status) => {
            run_status_command(&target).await?;
        }
        Some(Commands::Version) => println!("{}", env!("CARGO_PKG_VERSION")),
        Some(Commands::Setup) => {
            run_setup_flow(&mut console, &target, service_manager).await?;
        }
        Some(Commands::Reset) => {
            run_reset_command(&target).await?;
        }
        Some(Commands::SecretPath) => {
            run_secret_path_command(&target).await?;
        }
        Some(Commands::Account { ref command }) => match command {
            AccountCommands::LoginUrl => {
                run_account_login_url_command(&target).await?;
            }
        },
        Some(Commands::Claim { command }) => match command {
            ClaimCommands::Generate => {
                console.write_screen(claim_generate()).await;
            }
            ClaimCommands::Url { claim_code, .. } => {
                console
                    .write_screen(claim_url(&claim_code)?.to_string())
                    .await;
            }
            ClaimCommands::Exchange { claim_code, wait } => {
                let secret_key =
                    claim_exchange(&mut console, &claim_code, ClaimMode::SelfManaged, wait).await?;
                console.write_screen(secret_key).await;
            }
        },
    }

    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(target_os = "linux")]
fn service_manager_mode(cli: &Cli) -> ServiceManagerMode {
    match (cli.systemd, cli.openrc) {
        (true, false) => ServiceManagerMode::Systemd,
        (false, true) => ServiceManagerMode::OpenRc,
        (false, false) => ServiceManagerMode::None,
        (true, true) => unreachable!("clap conflicts_with prevents this"),
    }
}

#[cfg(target_os = "windows")]
fn service_manager_mode(_cli: &Cli) -> ServiceManagerMode {
    ServiceManagerMode::WindowsService
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn service_manager_mode(_cli: &Cli) -> ServiceManagerMode {
    ServiceManagerMode::Native
}

fn init_stdout_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    let log_filter =
        EnvFilter::try_from_env("PLAYIT_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stdout());
    tracing_subscriber::fmt()
        .with_ansi(current_platform() == RuntimePlatform::Linux)
        .with_writer(non_blocking)
        .with_env_filter(log_filter)
        .init();
    guard
}

pub async fn run_setup_flow(
    console: &mut ConsoleUi,
    target: &CliTarget,
    service_manager: ServiceManagerMode,
) -> Result<(), CliError> {
    let claim = begin_service_claim(console, target, service_manager).await?;
    let claim_code = claim.claim_code;
    console
        .write_screen(format!(
            "Open this link to finish setting up playit:\n{}",
            claim.claim_url
        ))
        .await;

    complete_service_claim(console, target, &claim_code).await?;

    if let Err(error) = run_account_login_url_command(target).await {
        tracing::debug!(?error, "account login URL is not ready after setup");
    }

    console
        .write_screen("playit setup is complete. The background service is ready.")
        .await;
    Ok(())
}

pub fn claim_generate() -> String {
    ClaimService::begin().code
}

pub fn claim_url(code: &str) -> Result<String, CliError> {
    if hex::decode(code).is_err() {
        return Err(CliError::InvalidClaimCode);
    }

    Ok(format!("https://playit.gg/claim/{}", code,))
}

pub async fn claim_exchange(
    console: &mut ConsoleUi,
    claim_code: &str,
    claim_mode: ClaimMode,
    wait_sec: u32,
) -> Result<String, CliError> {
    let claims = ClaimService::new(
        API_BASE.to_string(),
        format!("playit {}", env!("CARGO_PKG_VERSION")),
    );

    let end_at = if wait_sec == 0 {
        u64::MAX
    } else {
        now_milli() + (wait_sec as u64) * 1000
    };

    {
        let _close_guard = get_signal_handle().close_guard();
        let mut last_message = "Preparing setup...".to_string();

        loop {
            let setup_res = claims.progress(claim_code, claim_mode).await;

            let setup = match setup_res {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "Failed loading claim setup");
                    console
                        .write_screen(format!("{}\n\nError: {:?}", last_message, error))
                        .await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            last_message = match setup {
                ClaimProgress::WaitingForVisit => {
                    format!(
                        "Open this link to finish setting up playit:\n{}",
                        claim_url(claim_code)?
                    )
                }
                ClaimProgress::WaitingForApproval => {
                    format!(
                        "Approve this program in your browser:\n{}",
                        claim_url(claim_code)?
                    )
                }
                ClaimProgress::Approved => {
                    console
                        .write_screen("Program approved. Finishing setup...")
                        .await;
                    break;
                }
                ClaimProgress::Rejected => {
                    console
                        .write_screen("Setup was not approved in the browser.")
                        .await;
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    return Err(CliError::AgentClaimRejected);
                }
            };

            console.write_screen(&last_message).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    let secret_key = loop {
        match claims.exchange(claim_code).await {
            Ok(ClaimExchange::Accepted(secret)) => break secret,
            Ok(ClaimExchange::Pending(status)) => {
                let msg = format!(
                    "Waiting for claim code \"{}\" to be approved: {}",
                    claim_code, status
                );
                console.write_screen(msg).await;
            }
            Err(error) => return Err(CliError::ServiceError(error.detail)),
        };

        if now_milli() > end_at {
            console
                .write_screen("Setup timed out before the program was approved.")
                .await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            return Err(CliError::TimedOut);
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    };

    Ok(secret_key)
}

#[derive(Debug)]
pub enum CliError {
    InvalidClaimCode,
    NotImplemented,
    MissingSecret,
    MalformedSecret,
    InvalidSecret,
    RenderError(std::io::Error),
    SecretFileLoadError,
    SecretFileWriteError(std::io::Error),
    SecretFilePathMissing,
    InvalidPortType,
    InvalidPortCount,
    InvalidMappingOverride,
    AgentClaimRejected,
    InvalidConfigFile,
    TunnelNotFound(Uuid),
    TimedOut,
    AnswerNotProvided,
    TunnelOverwrittenAlready(Uuid),
    ResourceNotFoundAfterCreate(Uuid),
    TunnelSetupError(SetupError),
    ServiceError(String),
    IpcError(String),
}

impl Error for CliError {}

impl Display for CliError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ServiceError(message) | Self::IpcError(message) => {
                write!(f, "{message}")
            }
            _ => write!(f, "{:?}", self),
        }
    }
}

impl From<SetupError> for CliError {
    fn from(e: SetupError) -> Self {
        CliError::TunnelSetupError(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[cfg(target_os = "linux")]
    #[test]
    fn top_level_help_output_is_stable() {
        assert_eq!(
            Cli::command().render_help().to_string(),
            include_str!("../fixtures/help.txt")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn top_level_help_output_is_stable() {
        assert_eq!(
            Cli::command().render_help().to_string(),
            include_str!("../fixtures/help_non_linux.txt")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_manager_mode_defaults_to_none() {
        let cli = Cli::try_parse_from(["playit-cli"]).unwrap();

        assert_eq!(service_manager_mode(&cli), ServiceManagerMode::None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_manager_mode_accepts_systemd() {
        let cli = Cli::try_parse_from(["playit-cli", "--systemd"]).unwrap();

        assert_eq!(service_manager_mode(&cli), ServiceManagerMode::Systemd);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_manager_mode_accepts_openrc() {
        let cli = Cli::try_parse_from(["playit-cli", "--openrc"]).unwrap();

        assert_eq!(service_manager_mode(&cli), ServiceManagerMode::OpenRc);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_manager_flags_conflict() {
        let error = match Cli::try_parse_from(["playit-cli", "--systemd", "--openrc"]) {
            Ok(_) => panic!("expected --systemd and --openrc to conflict"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_service_manager_mode_uses_windows_service() {
        let cli = Cli::try_parse_from(["playit-cli"]).unwrap();

        assert_eq!(
            service_manager_mode(&cli),
            ServiceManagerMode::WindowsService
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_does_not_accept_linux_service_manager_flags() {
        assert!(Cli::try_parse_from(["playit-cli", "--systemd"]).is_err());
        assert!(Cli::try_parse_from(["playit-cli", "--openrc"]).is_err());
    }
}
