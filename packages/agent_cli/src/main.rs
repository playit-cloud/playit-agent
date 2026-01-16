use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;
use std::time::Duration;

use clap::{Parser, Subcommand};
use playit_agent_core::agent_control::platform::current_platform;
use playit_agent_core::agent_control::version::{help_register_version, register_platform};
use rand::Rng;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
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

/// Represents the service mode selection for the start command
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceMode {
    /// Auto-detect: check user service first, then system service
    #[cfg(not(target_os = "linux"))]
    Auto,
    /// Explicitly use user-level service (not available on Linux)
    #[cfg(not(target_os = "linux"))]
    User,
    /// Explicitly use system-level service
    System,
}

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

pub mod autorun;
pub mod playit_secret;
pub mod service;
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

    /// Start the playit agent (starts service and attaches to receive updates)
    Start {
        /// Run as system-wide service (requires admin/root)
        #[cfg(not(target_os = "linux"))]
        #[arg(long)]
        system: bool,

        /// Run as user service (default when starting new service)
        #[cfg(not(target_os = "linux"))]
        #[arg(long)]
        user: bool,

        /// Print logs to stdout instead of using TUI
        #[arg(short = 's', long)]
        stdout: bool,
    },

    /// Stop the background service
    Stop {
        /// Stop system-wide service (requires admin/root)
        #[cfg(not(target_os = "linux"))]
        #[arg(long)]
        system: bool,
    },

    /// Show the status of the background service
    Status {
        /// Check system-wide service status
        #[cfg(not(target_os = "linux"))]
        #[arg(long)]
        system: bool,
    },

    /// Install the playit agent as a system service
    Install {
        /// Install as system-wide service (requires admin/root)
        #[cfg(not(target_os = "linux"))]
        #[arg(long)]
        system: bool,
    },

    /// Uninstall the playit agent system service
    Uninstall {
        /// Uninstall system-wide service (requires admin/root)
        #[cfg(not(target_os = "linux"))]
        #[arg(long)]
        system: bool,
    },

    /// Run the agent directly in foreground (for Docker/debugging)
    RunEmbedded,

    /// Internal: Run as background service daemon
    #[command(hide = true)]
    RunService {
        /// Run as user service (not system-wide) - not available on Linux
        #[cfg(not(target_os = "linux"))]
        #[arg(long)]
        user: bool,
    },

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
    #[command(
        about = "Setting up a new playit agent",
        long_about = "Provides a URL that can be visited to claim the agent and generate a secret key"
    )]
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

    let mut secret =
        PlayitSecret::from_args(cli.secret.clone(), cli.secret_path.clone(), cli.secret_wait).await;
    let _ = secret.with_default_path().await;

    // Handle run-service command first - it sets up its own logging
    #[cfg(not(target_os = "linux"))]
    if let Some(Commands::RunService { user }) = &cli.command {
        let system_mode = !user;
        if let Err(e) = service::run_daemon(system_mode).await {
            eprintln!("Daemon error: {}", e);
            return Ok(std::process::ExitCode::FAILURE);
        }
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // On Linux, run-service always runs in system mode (no user-level service)
    #[cfg(target_os = "linux")]
    if let Some(Commands::RunService { .. }) = &cli.command {
        if let Err(e) = service::run_daemon(true).await {
            eprintln!("Daemon error: {}", e);
            return Ok(std::process::ExitCode::FAILURE);
        }
        return Ok(std::process::ExitCode::SUCCESS);
    }

    let log_only = cli.stdout;
    let log_path = cli.log_path.as_ref();

    // Check if Start command has --stdout flag
    let start_stdout = matches!(
        &cli.command,
        Some(Commands::Start { stdout: true, .. } | Commands::RunEmbedded)
    );

    // Use log-only mode if stdout flag is set OR if a log file path is specified OR if start --stdout
    let use_log_only = log_only || log_path.is_some() || start_stdout;

    // Create UI first so we can get its log capture
    let mut ui = UI::new(UISettings {
        auto_answer: None,
        log_only: use_log_only,
    });

    /* setup logging */
    // Get log level from PLAYIT_LOG env var, defaulting to "info"
    let log_filter =
        EnvFilter::try_from_env("PLAYIT_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    let _guard = match (use_log_only, log_path) {
        (true, Some(path)) => {
            // Log to file
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
            // Log to stdout (for -s flag, run-embedded, or start -s)
            let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stdout());
            tracing_subscriber::fmt()
                .with_ansi(current_platform() == Platform::Linux)
                .with_writer(non_blocking)
                .with_env_filter(log_filter)
                .init();
            Some(guard)
        }
        (false, Some(_)) => {
            panic!("log_path set but use_log_only is false - this shouldn't happen");
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
            // Default behavior: start service and attach (use TUI)
            #[cfg(not(target_os = "linux"))]
            {
                run_start_command(&mut ui, ServiceMode::Auto, false).await?;
            }
            #[cfg(target_os = "linux")]
            {
                run_start_command(&mut ui, ServiceMode::System, false).await?;
            }
        }
        #[cfg(not(target_os = "linux"))]
        Some(Commands::Start { system, user, stdout }) => {
            let mode = match (user, system) {
                (true, true) => {
                    return Err(CliError::ServiceError(
                        "Cannot specify both --user and --system".to_string(),
                    ));
                }
                (true, false) => ServiceMode::User,
                (false, true) => ServiceMode::System,
                (false, false) => ServiceMode::Auto,
            };
            run_start_command(&mut ui, mode, stdout).await?;
        }
        #[cfg(target_os = "linux")]
        Some(Commands::Start { stdout }) => {
            // On Linux, only system-level service is supported
            run_start_command(&mut ui, ServiceMode::System, stdout).await?;
        }
        #[cfg(not(target_os = "linux"))]
        Some(Commands::Stop { system }) => {
            run_stop_command(system).await?;
        }
        #[cfg(target_os = "linux")]
        Some(Commands::Stop { .. }) => {
            run_stop_command(true).await?;
        }
        #[cfg(not(target_os = "linux"))]
        Some(Commands::Status { system }) => {
            run_status_command(system).await?;
        }
        #[cfg(target_os = "linux")]
        Some(Commands::Status { .. }) => {
            run_status_command(true).await?;
        }
        #[cfg(not(target_os = "linux"))]
        Some(Commands::Install { system }) => {
            run_install_command(system)?;
        }
        #[cfg(target_os = "linux")]
        Some(Commands::Install { .. }) => {
            run_install_command(true)?;
        }
        #[cfg(not(target_os = "linux"))]
        Some(Commands::Uninstall { system }) => {
            run_uninstall_command(system)?;
        }
        #[cfg(target_os = "linux")]
        Some(Commands::Uninstall { .. }) => {
            run_uninstall_command(true)?;
        }
        Some(Commands::RunEmbedded) => {
            // Run agent directly without TUI, printing logs to stdout
            let mut embedded_ui = UI::new(UISettings {
                auto_answer: Some(true),
                log_only: true,
            });
            autorun(&mut embedded_ui, secret).await?;
        }
        Some(Commands::RunService { .. }) => {
            // Handled above before logging setup
            unreachable!("RunService is handled before logging setup");
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
            )
            .await;
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
            )
            .await;
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

/// Run the start command: start service and attach to receive updates
async fn run_start_command(
    ui: &mut UI,
    mode: ServiceMode,
    stdout_mode: bool,
) -> Result<(), CliError> {
    use crate::service::ipc::{IpcClient, ServiceEvent};
    use crate::service::manager::ensure_service_running;

    // Determine which service mode to use based on what's running
    #[cfg(not(target_os = "linux"))]
    let system_mode = match mode {
        ServiceMode::User => false,
        ServiceMode::System => true,
        ServiceMode::Auto => {
            // Check user service first, then system service
            ui.write_screen("Checking for running services...").await;

            if IpcClient::is_running(false).await {
                tracing::info!("Found running user service");
                false
            } else if IpcClient::is_running(true).await {
                tracing::info!("Found running system service");
                true
            } else {
                // Neither is running, default to user mode
                tracing::info!("No running service found, will start user service");
                false
            }
        }
    };

    // On Linux, only system-level service is supported (via package manager's systemd unit)
    #[cfg(target_os = "linux")]
    let system_mode = {
        let _ = mode; // silence unused variable warning
        true
    };

    // Ensure service is running
    ui.write_screen("Ensuring playit service is running...")
        .await;

    if let Err(e) = ensure_service_running(system_mode).await {
        return Err(CliError::ServiceError(format!(
            "Failed to start service: {}",
            e
        )));
    }

    let mode_str = if system_mode { "system" } else { "user" };
    ui.write_screen(format!("Service is running ({})", mode_str))
        .await;

    // Connect to service via IPC
    ui.write_screen("Connecting to service...").await;

    let mut client = match IpcClient::connect(system_mode).await {
        Ok(client) => client,
        Err(e) => {
            return Err(CliError::IpcError(format!(
                "Failed to connect to service: {}",
                e
            )));
        }
    };

    // Subscribe to updates
    if let Err(e) = client.subscribe().await {
        return Err(CliError::IpcError(format!("Failed to subscribe: {}", e)));
    }

    tracing::info!("Connected to service, receiving updates");

    // Main loop: receive events and update UI
    loop {
        tokio::select! {
            event_result = client.recv_event() => {
                match event_result {
                    Ok(event) => {
                        match event {
                            ServiceEvent::AgentData { .. } => {
                                if !stdout_mode {
                                    if let Some(data) = event.to_agent_data() {
                                        ui.update_agent_data(data);
                                    }
                                }
                            }
                            ServiceEvent::Stats { .. } => {
                                if !stdout_mode {
                                    if let Some(stats) = event.to_connection_stats() {
                                        ui.update_stats(stats);
                                    }
                                }
                            }
                            ServiceEvent::Log { level, target, message, timestamp } => {
                                if stdout_mode {
                                    // Print log in tracing format
                                    let formatted_ts = format_timestamp_millis(timestamp);
                                    println!("{} {:>5} {}: {}", formatted_ts, level.to_uppercase(), target, message);
                                } else if let Some(log_capture) = ui.log_capture() {
                                    use crate::ui::log_capture::{LogEntry, LogLevel};
                                    let log_level = match level.as_str() {
                                        "error" | "ERROR" => LogLevel::Error,
                                        "warn" | "WARN" => LogLevel::Warn,
                                        "info" | "INFO" => LogLevel::Info,
                                        "debug" | "DEBUG" => LogLevel::Debug,
                                        _ => LogLevel::Trace,
                                    };
                                    log_capture.push(LogEntry {
                                        level: log_level,
                                        target,
                                        message,
                                        timestamp,
                                    });
                                }
                            }
                            ServiceEvent::Status { .. } => {
                                // Status updates are handled separately
                            }
                            ServiceEvent::Ack { .. } | ServiceEvent::Error { .. } => {
                                // Acknowledgements handled in specific commands
                            }
                        }
                    }
                    Err(e) => {
                        if stdout_mode {
                            eprintln!("Connection to service lost: {}", e);
                        } else {
                            tracing::error!("IPC error: {}", e);
                            ui.write_screen(format!("Connection to service lost: {}", e)).await;
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        break;
                    }
                }
            }
            // Handle TUI tick (only when not in stdout mode)
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if !stdout_mode && ui.is_tui() {
                    match ui.tick_tui() {
                        Ok(true) => {} // Continue
                        Ok(false) => {
                            // Quit requested - just detach, don't stop service
                            ui.shutdown_tui()?;
                            println!("Detached from service. Service continues running in background.");
                            println!("Use '{} stop' to stop the service.", *EXE_NAME);
                            break;
                        }
                        Err(e) => {
                            ui.shutdown_tui()?;
                            return Err(e);
                        }
                    }
                }
            }
            // Handle Ctrl+C
            _ = tokio::signal::ctrl_c() => {
                if !stdout_mode && ui.is_tui() {
                    ui.shutdown_tui()?;
                }
                println!("\nDetached from service. Service continues running in background.");
                println!("Use '{} stop' to stop the service.", *EXE_NAME);
                break;
            }
        }
    }

    Ok(())
}

/// Run the stop command
async fn run_stop_command(system_mode: bool) -> Result<(), CliError> {
    use crate::service::ipc::IpcClient;

    // First try to stop via IPC
    if let Ok(mut client) = IpcClient::connect(system_mode).await {
        match client.stop().await {
            Ok(_) => {
                println!("Service stop requested");
                // Wait a bit for service to stop
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => {
                tracing::warn!("Failed to send stop via IPC: {}", e);
            }
        }
    }

    // On Linux, use systemctl to stop the service (only system-level is supported)
    #[cfg(target_os = "linux")]
    {
        let _ = system_mode; // silence unused variable warning
        if let Err(e) = service::manager::stop_systemd_service() {
            tracing::warn!("Failed to stop via systemctl: {}", e);
        }
    }

    // On non-Linux, try via service manager
    #[cfg(not(target_os = "linux"))]
    {
        match service::ServiceController::new(system_mode) {
            Ok(controller) => {
                if let Err(e) = controller.stop() {
                    tracing::warn!("Failed to stop via service manager: {}", e);
                }
            }
            Err(e) => {
                tracing::debug!("Service manager not available: {}", e);
            }
        }
    }

    // Verify service stopped
    tokio::time::sleep(Duration::from_millis(500)).await;
    if !IpcClient::is_running(system_mode).await {
        println!("Service stopped");
    } else {
        println!("Service may still be running");
    }

    Ok(())
}

/// Run the status command
async fn run_status_command(system_mode: bool) -> Result<(), CliError> {
    use crate::service::ipc::IpcClient;

    if !IpcClient::is_running(system_mode).await {
        println!("Service is not running");
        return Ok(());
    }

    let mut client = match IpcClient::connect(system_mode).await {
        Ok(client) => client,
        Err(e) => {
            println!("Service appears to be running but cannot connect: {}", e);
            return Ok(());
        }
    };

    match client.status().await {
        Ok(service::ipc::ServiceEvent::Status {
            running,
            pid,
            uptime_secs,
        }) => {
            println!("Service status:");
            println!("  Running: {}", running);
            println!("  PID: {}", pid);
            println!("  Uptime: {} seconds", uptime_secs);
        }
        Ok(other) => {
            println!("Unexpected response: {:?}", other);
        }
        Err(e) => {
            println!("Failed to get status: {}", e);
        }
    }

    Ok(())
}

/// Run the install command
#[cfg(target_os = "linux")]
fn run_install_command(_system_mode: bool) -> Result<(), CliError> {
    // On Linux, the service is managed by the package manager
    Err(CliError::ServiceError(
        "The playit service is managed by the package manager. Use your system package manager to install or uninstall the service.".to_string()
    ))
}

/// Run the install command
#[cfg(not(target_os = "linux"))]
fn run_install_command(system_mode: bool) -> Result<(), CliError> {
    let controller = service::ServiceController::new(system_mode)
        .map_err(|e| CliError::ServiceError(e.to_string()))?;

    controller
        .install()
        .map_err(|e| CliError::ServiceError(e.to_string()))?;

    let mode_str = if system_mode { "system" } else { "user" };
    println!("Service installed successfully ({} mode)", mode_str);
    println!("Use '{} start' to start the service", *EXE_NAME);

    Ok(())
}

/// Run the uninstall command
#[cfg(target_os = "linux")]
fn run_uninstall_command(_system_mode: bool) -> Result<(), CliError> {
    // On Linux, the service is managed by the package manager
    Err(CliError::ServiceError(
        "The playit service is managed by the package manager. Use your system package manager to install or uninstall the service.".to_string()
    ))
}

/// Run the uninstall command
#[cfg(not(target_os = "linux"))]
fn run_uninstall_command(system_mode: bool) -> Result<(), CliError> {
    let controller = service::ServiceController::new(system_mode)
        .map_err(|e| CliError::ServiceError(e.to_string()))?;

    // Try to stop first
    let _ = controller.stop();

    controller
        .uninstall()
        .map_err(|e| CliError::ServiceError(e.to_string()))?;

    println!("Service uninstalled successfully");

    Ok(())
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

/// Format a timestamp in milliseconds since epoch to RFC3339 format (like tracing uses)
fn format_timestamp_millis(millis: u64) -> String {
    use chrono::{DateTime, Utc};

    DateTime::<Utc>::from_timestamp_millis(millis as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
        .unwrap_or_else(|| format!("{}ms", millis))
}
