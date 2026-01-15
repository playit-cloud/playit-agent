use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;
use std::time::Duration;

use clap::{Parser, Subcommand};
use playit_agent_core::agent_control::platform::current_platform;
use playit_agent_core::agent_control::version::{help_register_version, register_platform};
use rand::Rng;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use autorun::autorun;
use playit_agent_core::agent_control::errors::SetupError;
use playit_agent_core::utils::now_milli;
use playit_api_client::http_client::HttpClientError;
use playit_api_client::{PlayitApi, api::*};
use playit_secret::PlayitSecret;

use crate::signal_handle::get_signal_handle;
use crate::ui::log_capture::LogCaptureLayer;
use crate::ui::{UI, UISettings};

pub static API_BASE: LazyLock<String> =
    LazyLock::new(|| dotenv::var("API_BASE").unwrap_or("https://api.playit.gg".to_string()));

pub mod autorun;
pub mod playit_secret;
pub mod signal_handle;
pub mod ui;
pub mod util;

#[derive(Parser)]
#[command(name = "playit-cli")]
struct Cli {
    /// Secret code for the agent
    #[arg(long)]
    secret: Option<String>,

    /// Path to file containing secret
    #[arg(long)]
    secret_path: Option<String>,

    /// Wait for secret_path file to read secret
    #[arg(short = 'w', long)]
    secret_wait: bool,

    /// Prints logs to stdout
    #[arg(short = 's', long)]
    stdout: bool,

    /// Path to write logs to
    #[arg(short = 'l', long)]
    log_path: Option<String>,

    /// Overrides platform in version to be docker
    #[arg(long)]
    platform_docker: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Print version information
    Version,

    /// Start the playit agent
    Start,

    /// Removes the secret key on your system so the playit agent can be re-claimed
    Reset,

    /// Shows the file path where the playit secret can be found
    SecretPath,

    #[cfg(target_os = "linux")]
    /// Setup playit for Linux service
    Setup,

    /// Account management commands
    Account {
        #[command(subcommand)]
        command: AccountCommands,
    },

    /// Setting up a new playit agent
    #[command(about = "Setting up a new playit agent", long_about = "Provides a URL that can be visited to claim the agent and generate a secret key")]
    Claim {
        #[command(subcommand)]
        command: ClaimCommands,
    },

    /// Manage tunnels
    Tunnels {
        #[command(subcommand)]
        command: TunnelCommands,
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

#[derive(Subcommand)]
enum TunnelCommands {
    /// Create a tunnel if it doesn't exist with the parameters
    Prepare {
        /// Either "tcp", "udp", or "both"
        port_type: String,

        /// Number of ports in a series to allocate
        #[arg(default_value = "1")]
        port_count: String,

        /// The tunnel type
        #[arg(long)]
        r#type: Option<String>,

        /// Name of the tunnel
        #[arg(long)]
        name: Option<String>,

        #[arg(long)]
        exact: bool,

        #[arg(long)]
        ignore_name: bool,
    },

    /// List tunnels (format "[tunnel-id] [port-type] [port-count] [public-address]")
    List,
}

#[tokio::main]
async fn main() -> Result<std::process::ExitCode, CliError> {
    let cli = Cli::parse();

    /* register docker */
    {
        let platform = if cli.platform_docker {
            Platform::Docker
        } else {
            current_platform()
        };

        register_platform(platform);

        help_register_version(
            env!("CARGO_PKG_VERSION"),
            "308943e8-faef-4835-a2ba-270351f72aa3",
        );
    }

    let mut secret = PlayitSecret::from_args(
        cli.secret.clone(),
        cli.secret_path.clone(),
        cli.secret_wait,
    ).await;
    let _ = secret.with_default_path().await;

    let log_only = cli.stdout;
    let log_path = cli.log_path.as_ref();

    // Use log-only mode if stdout flag is set OR if a log file path is specified
    let use_log_only = log_only || log_path.is_some();

    // Create UI first so we can get its log capture
    let mut ui = UI::new(UISettings {
        auto_answer: None,
        log_only: use_log_only,
    });

    /* setup logging */
    // Get log level from PLAYIT_LOG env var, defaulting to "info"
    let log_filter = EnvFilter::try_from_env("PLAYIT_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let _guard = match (log_only, log_path) {
        (true, Some(_)) => panic!("try to use -s and -l at the same time"),
        (false, Some(path)) => {
            let write_path = match path.rsplit_once("/") {
                Some((dir, file)) => tracing_appender::rolling::never(dir, file),
                None => tracing_appender::rolling::never(".", path),
            };

            let (non_blocking, guard) = tracing_appender::non_blocking(write_path);
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(non_blocking)
                .with_env_filter(log_filter)
                .init();
            Some(guard)
        }
        (true, None) => {
            let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stdout());
            tracing_subscriber::fmt()
                .with_ansi(current_platform() == Platform::Linux)
                .with_writer(non_blocking)
                .with_env_filter(log_filter)
                .init();
            Some(guard)
        }
        (false, None) => {
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
            ui.write_screen("no command provided, doing auto run").await;
            tokio::time::sleep(Duration::from_secs(1)).await;
            autorun(&mut ui, secret).await?;
        }
        Some(Commands::Start) => {
            autorun(&mut ui, secret).await?;
        }
        Some(Commands::Version) => println!("{}", env!("CARGO_PKG_VERSION")),
        #[cfg(target_os = "linux")]
        Some(Commands::Setup) => {
            let mut secret = PlayitSecret::linux_service();
            let key = secret
                .ensure_valid(&mut ui)
                .await?
                .get_or_setup(&mut ui)
                .await?;

            let api = PlayitApi::create(API_BASE.to_string(), Some(key));
            if let Ok(session) = api.login_guest().await {
                ui.write_screen(format!(
                    "Guest login:\nhttps://playit.gg/login/guest-account/{}",
                    session.session_key
                ))
                .await;
                tokio::time::sleep(Duration::from_secs(10)).await;
            }

            ui.write_screen("Playit setup, secret written to /etc/playit/playit.toml")
                .await;
        }
        Some(Commands::Reset) => loop {
            let mut secrets = PlayitSecret::from_args(
                cli.secret.clone(),
                cli.secret_path.clone(),
                cli.secret_wait,
            ).await;
            secrets.with_default_path().await;

            let path = secrets.get_path().unwrap();
            if !tokio::fs::try_exists(path).await.unwrap_or(false) {
                break;
            }

            tokio::fs::remove_file(path).await.unwrap();
            println!("deleted secret at: {}", path);
        },
        Some(Commands::SecretPath) => {
            let mut secrets = PlayitSecret::from_args(
                cli.secret.clone(),
                cli.secret_path.clone(),
                cli.secret_wait,
            ).await;
            secrets.with_default_path().await;
            let path = secrets.get_path().unwrap();
            println!("{}", path);
        }
        Some(Commands::Account { command }) => match command {
            AccountCommands::LoginUrl => {
                let api = secret.create_api().await?;
                let session = api.login_guest().await?;
                println!(
                    "https://playit.gg/login/guest-account/{}",
                    session.session_key
                )
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
        Some(Commands::Tunnels { command }) => match command {
            TunnelCommands::Prepare { .. } => {
                return Err(CliError::NotImplemented);
            }
            TunnelCommands::List => {
                return Err(CliError::NotImplemented);
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
                    version: format!("playit-cli {}", env!("CARGO_PKG_VERSION")),
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
}

impl Error for CliError {}

impl Display for CliError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
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
