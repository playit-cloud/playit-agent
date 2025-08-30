use clap::{Parser, Subcommand, ValueEnum};
use playit_api_client::api::AgentVersion;
use std::str::FromStr;
use uuid::Uuid;

#[derive(Parser)]
pub struct Cli {
    #[arg(long)]
    pub secret_key: Option<String>,
    #[arg(long)]
    pub secret_key_path: Option<String>,
    #[arg(long)]
    pub settings_path: Option<String>,
    #[arg(long)]
    pub override_variant_id: Option<Uuid>,
    #[arg(long)]
    pub override_variant_version: Option<String>,
    #[arg(long)]
    pub override_variant_path: Option<String>,
    #[arg(long)]
    pub display: Option<DisplayMode>,
    #[command(subcommand)]
    pub command: CliCommand,
}

#[derive(Clone, Subcommand)]
pub enum CliCommand {
    #[command(name = "version")]
    Version,
    #[command(name = "guest-login-url")]
    GuestLoginUrl,
    #[command(name = "reset")]
    ResetSecretPaths,
    #[command(name = "secret-path")]
    RevealSecretPath,
    #[command(name = "start")]
    Start,
    #[command(name = "stop")]
    Stop,
    #[command(name = "run")]
    Run,
    #[command(name = "monitor")]
    Monitor,
    #[command(name = "status")]
    Status,
}

#[derive(Debug)]
pub enum CliAction {
    Version,
    GuestLoginUrl,
    ResetSecretPaths,
    RevealSecretPath,
    Start {
        foreground: bool,
        secret: Option<CliSecretDetails>,
        version: Option<CliAgentVersionDetails>,
    },
    Stop,
    Status,
    Monitor,
}

impl CliAction {
    pub fn from_cli(cli: Cli) -> Result<Self, CliError> {
        Ok(match &cli.command {
            CliCommand::Version => Self::Version,
            CliCommand::GuestLoginUrl => Self::GuestLoginUrl,
            CliCommand::ResetSecretPaths => Self::ResetSecretPaths,
            CliCommand::RevealSecretPath => Self::RevealSecretPath,
            CliCommand::Start => Self::Start {
                foreground: false,
                secret: CliSecretDetails::extract(&cli)?,
                version: CliAgentVersionDetails::extract(&cli)?,
            },
            CliCommand::Run => Self::Start {
                foreground: true,
                secret: CliSecretDetails::extract(&cli)?,
                version: CliAgentVersionDetails::extract(&cli)?,
            },
            CliCommand::Stop => Self::Stop,
            CliCommand::Status => Self::Status,
            CliCommand::Monitor => Self::Monitor,
        })
    }
}

#[derive(ValueEnum, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    #[value(name = "foreground")]
    Foreground,
    #[value(name = "daemon")]
    Daemon,
}

#[derive(ValueEnum, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    #[value(name = "verbose")]
    LogsVerbose,
    #[value(name = "info")]
    InfoLogs,
    #[value(name = "ui")]
    TermUi,
}

#[derive(Debug)]
pub struct CliAgentVersionDetails {
    pub agent_version: AgentVersion,
}

impl CliAgentVersionDetails {
    pub fn extract(cli: &Cli) -> Result<Option<Self>, CliError> {
        if let Some(variant) = cli.override_variant_id {
            let Some(semvar) = &cli.override_variant_version else {
                return Err(CliError::ConflictingArgs {
                    first: "override_variant_id".to_string(),
                    second: "override_variant_version".to_string(),
                    reason: "variant_version must be supplied if overriding variant id".to_string(),
                });
            };

            if cli.override_variant_path.is_some() {
                return Err(CliError::ConflictingArgs {
                    first: "override_variant_id".to_string(),
                    second: "override_variant_path".to_string(),
                    reason: "both cannot be defined".to_string(),
                });
            }

            let mut version_parts = semvar.split(".");
            let a = version_parts.next().and_then(|s| u32::from_str(s).ok());
            let b = version_parts.next().and_then(|s| u32::from_str(s).ok());
            let c = version_parts.next().and_then(|s| u32::from_str(s).ok());

            let agent_version = match (a, b, c) {
                (Some(a), Some(b), Some(c)) => AgentVersion {
                    variant_id: variant,
                    version_major: a,
                    version_minor: b,
                    version_patch: c,
                },
                _ => {
                    return Err(CliError::InvalidArgFormat {
                        resource_name: "override_variant_version".to_string(),
                        expected_format: "<major:u32>.<minor:u32>.<patch:u32>".to_string(),
                    });
                }
            };

            return Ok(Some(CliAgentVersionDetails { agent_version }));
        }

        if let Some(path) = &cli.override_variant_path {
            let json = std::fs::read_to_string(path).map_err(|io_error| {
                CliError::FailedToLoadFileContent {
                    resource_name: "override_variant_path".to_string(),
                    file_path: path.to_string(),
                    io_error,
                }
            })?;

            let agent_version = serde_json::from_str::<AgentVersion>(&json).map_err(|_| {
                CliError::InvalidArgFormat {
                    resource_name: "override_variant_path".to_string(),
                    expected_format: "<agent_version:json>".to_string(),
                }
            })?;

            return Ok(Some(CliAgentVersionDetails { agent_version }));
        }

        Ok(None)
    }
}

#[derive(Debug)]
pub struct CliSecretDetails {
    pub secret_key: String,
}

impl CliSecretDetails {
    pub fn extract(cli: &Cli) -> Result<Option<Self>, CliError> {
        if let Some(secret_key) = &cli.secret_key {
            if cli.secret_key_path.is_some() {
                return Err(CliError::ConflictingArgs {
                    first: "secret_key".to_string(),
                    second: "secret_key_path".to_string(),
                    reason: "both cannot be defined".to_string(),
                });
            }

            return Ok(Some(Self {
                secret_key: secret_key.clone(),
            }));
        }

        if let Some(path) = &cli.secret_key_path {
            return match std::fs::read_to_string(path.as_str()) {
                Err(error) => Err(CliError::FailedToLoadFileContent {
                    resource_name: "secret_key_path".to_string(),
                    file_path: path.to_string(),
                    io_error: error,
                }),
                Ok(content) => Ok(Some(Self {
                    secret_key: content,
                })),
            };
        }

        Ok(None)
    }
}

#[derive(Debug)]
pub enum CliError {
    ConflictingArgs {
        first: String,
        second: String,
        reason: String,
    },
    FailedToLoadFileContent {
        resource_name: String,
        file_path: String,
        io_error: std::io::Error,
    },
    InvalidArgFormat {
        resource_name: String,
        expected_format: String,
    },
}

impl CliError {
    pub fn print_and_exit(self) -> ! {
        match self {
            Self::ConflictingArgs {
                first,
                second,
                reason,
            } => {
                eprintln!("Conflicting cli arguments / settings");
                eprintln!("{first} & {second}\n{reason}");
            }
            Self::FailedToLoadFileContent {
                resource_name,
                file_path,
                io_error,
            } => {
                eprintln!("Failed to load file {file_path:?}");
                eprintln!("Loading file for {resource_name}, error:\n{io_error:?}");
            }
            Self::InvalidArgFormat {
                resource_name,
                expected_format,
            } => {
                eprintln!("Invalid cli argument / setting");
                eprintln!("{resource_name} should be in format: {expected_format}");
            }
        }
        std::process::exit(1)
    }
}
