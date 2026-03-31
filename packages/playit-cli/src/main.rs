use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;
use std::time::Duration;

use clap::{Parser, Subcommand};
use client::{
    CliTarget, ensure_service_waiting_for_secret, provision_service_secret,
    run_account_login_url_command, run_attach_command, run_default_attach_command,
    run_reset_command, run_secret_path_command, run_start_command, run_status_command,
    run_stop_command,
};
use playit_agent_core::agent_control::platform::current_platform;
use playit_agent_core::agent_control::version::{help_register_version, register_platform};
use rand::Rng;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use uuid::Uuid;

use playit_agent_core::agent_control::errors::SetupError;
use playit_agent_core::utils::now_milli;
use playit_api_client::http_client::HttpClientError;
use playit_api_client::{PlayitApi, api::*};

use crate::signal_handle::get_signal_handle;
use crate::ui::log_capture::LogCaptureLayer;
use crate::ui::{UI, UISettings};

pub static API_BASE: LazyLock<String> =
    LazyLock::new(|| dotenv::var("API_BASE").unwrap_or("https://api.playit.gg".to_string()));

/// The name of the executable as invoked by the user
pub static EXE_NAME: LazyLock<String> = LazyLock::new(|| {
    std::env::args()
        .next()
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "playit".to_string())
});

mod client;
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
    match run_cli().await {
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

    let log_only = cli.stdout;
    let target = CliTarget::from_socket_path(cli.socket_path.clone());

    // Check if attach has --stdout flag
    let attach_stdout = matches!(&cli.command, Some(Commands::Attach { stdout: true, .. }));

    // Use log-only mode if stdout flag is set or if attach --stdout was requested.
    let use_log_only = log_only || attach_stdout;

    // Create UI first so we can get its log capture
    let mut ui = UI::new(UISettings {
        auto_answer: None,
        log_only: use_log_only,
    });

    /* setup logging */
    // Get log level from PLAYIT_LOG env var, defaulting to "info"
    let log_filter =
        EnvFilter::try_from_env("PLAYIT_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    let _guard = match use_log_only {
        true => {
            // Log to stdout for `-s` or `attach -s`.
            let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stdout());
            tracing_subscriber::fmt()
                .with_ansi(current_platform() == Platform::Linux)
                .with_writer(non_blocking)
                .with_env_filter(log_filter)
                .init();
            Some(guard)
        }
        false => {
            // TUI mode - set up log capture layer with filter
            if let Some(log_capture) = ui.log_capture() {
                let capture_layer = LogCaptureLayer::new(log_capture);
                tracing_subscriber::registry()
                    .with(log_filter)
                    .with(capture_layer)
                    .init();
            }
            None
        }
    };

    match cli.command {
        None => {
            run_default_attach_command(&mut ui, &target).await?;
        }
        Some(Commands::Attach { stdout }) => {
            run_attach_command(&mut ui, stdout, &target).await?;
        }
        Some(Commands::Start) => {
            run_start_command(&target).await?;
        }
        Some(Commands::Stop) => {
            run_stop_command(&target).await?;
        }
        Some(Commands::Status) => {
            run_status_command(&target).await?;
        }
        Some(Commands::Version) => println!("{}", env!("CARGO_PKG_VERSION")),
        Some(Commands::Setup) => {
            ensure_service_waiting_for_secret(&target).await?;

            let claim_code = claim_generate();
            ui.write_screen(format!("Visit link to setup {}", claim_url(&claim_code)?))
                .await;

            let key = claim_exchange(&mut ui, &claim_code, ClaimAgentType::Assignable, 0).await?;
            provision_service_secret(&target, &key).await?;

            let api = PlayitApi::create(API_BASE.to_string(), Some(key));
            if let Ok(session) = api.login_guest().await {
                ui.write_screen(format!(
                    "Guest login:\nhttps://playit.gg/login/guest-account/{}",
                    session.session_key
                ))
                .await;
                tokio::time::sleep(Duration::from_secs(10)).await;
            }

            ui.write_screen("Playit setup complete, secret provisioned to playitd")
                .await;
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
                ui.write_screen(claim_generate()).await;
            }
            ClaimCommands::Url { claim_code, .. } => {
                ui.write_screen(claim_url(&claim_code)?.to_string()).await;
            }
            ClaimCommands::Exchange { claim_code, wait } => {
                let secret_key =
                    claim_exchange(&mut ui, &claim_code, ClaimAgentType::SelfManaged, wait).await?;
                ui.write_screen(secret_key).await;
            }
        },
    }

    Ok(std::process::ExitCode::SUCCESS)
}

pub fn claim_generate() -> String {
    let mut buffer = [0u8; 5];
    rand::rng().fill(&mut buffer);
    hex::encode(&buffer)
}

pub fn claim_url(code: &str) -> Result<String, CliError> {
    if hex::decode(code).is_err() {
        return Err(CliError::InvalidClaimCode);
    }

    Ok(format!("https://playit.gg/claim/{}", code,))
}

pub async fn claim_exchange(
    ui: &mut UI,
    claim_code: &str,
    agent_type: ClaimAgentType,
    wait_sec: u32,
) -> Result<String, CliError> {
    let api = PlayitApi::create(API_BASE.to_string(), None);

    let end_at = if wait_sec == 0 {
        u64::MAX
    } else {
        now_milli() + (wait_sec as u64) * 1000
    };

    {
        let _close_guard = get_signal_handle().close_guard();
        let mut last_message = "Preparing Setup".to_string();

        loop {
            let setup_res = api
                .claim_setup(ReqClaimSetup {
                    code: claim_code.to_string(),
                    agent_type,
                    version: format!("{} {}", *EXE_NAME, env!("CARGO_PKG_VERSION")),
                })
                .await;

            let setup = match setup_res {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "Failed loading claim setup");
                    ui.write_screen(format!("{}\n\nError: {:?}", last_message, error))
                        .await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            last_message = match setup {
                ClaimSetupResponse::WaitingForUserVisit => {
                    format!("Visit link to setup {}", claim_url(claim_code)?)
                }
                ClaimSetupResponse::WaitingForUser => {
                    format!("Approve program at {}", claim_url(claim_code)?)
                }
                ClaimSetupResponse::UserAccepted => {
                    ui.write_screen("Program approved :). Secret code being setup.")
                        .await;
                    break;
                }
                ClaimSetupResponse::UserRejected => {
                    ui.write_screen("Program rejected :(").await;
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    return Err(CliError::AgentClaimRejected);
                }
            };

            ui.write_screen(&last_message).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    let secret_key = loop {
        match api
            .claim_exchange(ReqClaimExchange {
                code: claim_code.to_string(),
            })
            .await
        {
            Ok(res) => break res.secret_key,
            Err(ApiError::Fail(status)) => {
                let msg = format!("code \"{}\" not ready, {:?}", claim_code, status);
                ui.write_screen(msg).await;
            }
            Err(error) => return Err(error.into()),
        };

        if now_milli() > end_at {
            ui.write_screen("you took too long to approve the program, closing")
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
    RequestError(HttpClientError),
    ApiError(ApiResponseError),
    ApiFail(String),
    TunnelSetupError(SetupError),
    ServiceError(String),
    IpcError(String),
}

impl Error for CliError {}

impl Display for CliError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ServiceError(message) | Self::IpcError(message) | Self::ApiFail(message) => {
                write!(f, "{message}")
            }
            _ => write!(f, "{:?}", self),
        }
    }
}

impl<F: serde::Serialize> From<ApiError<F, HttpClientError>> for CliError {
    fn from(e: ApiError<F, HttpClientError>) -> Self {
        match e {
            ApiError::ApiError(e) => CliError::ApiError(e),
            ApiError::ClientError(e) => CliError::RequestError(e),
            ApiError::Fail(fail) => CliError::ApiFail(serde_json::to_string(&fail).unwrap()),
        }
    }
}

impl From<ApiErrorNoFail<HttpClientError>> for CliError {
    fn from(e: ApiErrorNoFail<HttpClientError>) -> Self {
        match e {
            ApiErrorNoFail::ApiError(e) => CliError::ApiError(e),
            ApiErrorNoFail::ClientError(e) => CliError::RequestError(e),
        }
    }
}

impl From<SetupError> for CliError {
    fn from(e: SetupError) -> Self {
        CliError::TunnelSetupError(e)
    }
}


