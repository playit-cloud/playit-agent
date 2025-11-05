use clap::{Parser, Subcommand, ValueEnum};
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
