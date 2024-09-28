use std::error::Error;
use std::fmt::{Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use args::{CliArgs, CmdAccount, CmdAgentType, CmdClaim, CmdTunnelRegion, CmdTunnelType, CmdTunnels, Commands};
use clap::Parser;
use playit_agent_core::agent_control::platform::get_platform;
use playit_agent_core::agent_control::version::register_version;
use playit_api_client::ip_resource::PlayitRegion;
use rand::Rng;
use uuid::Uuid;

use autorun::autorun;
use playit_api_client::{api::*, PlayitApi};
use playit_api_client::http_client::HttpClientError;
use playit_agent_core::network::address_lookup::{AddressLookup, AddressValue};
use playit_agent_core::agent_control::errors::SetupError;
use playit_agent_core::utils::now_milli;
use playit_secret::PlayitSecret;

use crate::match_ip::MatchIp;
use crate::signal_handle::get_signal_handle;
use crate::ui::{UI, UISettings};

pub const API_BASE: &'static str = "https://api.playit.gg";

pub mod util;
pub mod autorun;
pub mod playit_secret;
pub mod match_ip;
pub mod ui;
pub mod signal_handle;
pub mod args;

#[tokio::main]
async fn main() -> Result<std::process::ExitCode, CliError> {
    let mut args = CliArgs::parse();

    /* register docker */
    {
        let platform = if args.platform_docker {
            Platform::Docker
        } else {
            get_platform()
        };

        register_version(PlayitAgentVersion {
            version: AgentVersion {
                platform,
                version: env!("CARGO_PKG_VERSION").to_string(),
                has_expired: false,
            },
            official: true,
            details_website: None,
        });
    }

    let mut secret = PlayitSecret::from_args(&mut args).await;
    let _ = secret.with_default_path().await;

    let log_only = args.stdout;
    let log_path = args.log_path;

    /* setup logging */
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
                .init();
            Some(guard)
        }
        (true, None) => {
            let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stdout());
            tracing_subscriber::fmt()
                .with_ansi(get_platform() == Platform::Linux)
                .with_writer(non_blocking)
                .init();
            Some(guard)
        }
        _ => None,
    };

    let mut ui = UI::new(UISettings {
        auto_answer: None,
        log_only,
    });

    match args.cmd {
        #[cfg(target_os = "linux")]
        Commands::Setup => {
            let mut secret = PlayitSecret::linux_service();
            let key = secret
                .ensure_valid(&mut ui).await?
                .get_or_setup(&mut ui).await?;

            let api = PlayitApi::create(API_BASE.to_string(), Some(key));
            if let Ok(session) = api.login_guest().await {
                ui.write_screen(format!("Guest login:\nhttps://playit.gg/login/guest-account/{}", session.session_key)).await;
                tokio::time::sleep(Duration::from_secs(10)).await;
            }

            ui.write_screen("Playit setup, secret written to /etc/playit/playit.toml").await;
        }
        Commands::Start => autorun(&mut ui, secret).await?,
        Commands::Version => println!("{}", env!("CARGO_PKG_VERSION")),
        Commands::Reset => {
            for i in 0..100 {
                assert!(i < 30);

                secret.with_default_path().await;

                let path = secret.get_path().unwrap();
                if !tokio::fs::try_exists(path).await.unwrap_or(false) {
                    break;
                }

                tokio::fs::remove_file(path).await.unwrap();
                println!("deleted secret at: {}", path);
            }
        }
        Commands::SecretPath => {
            secret.with_default_path().await;
            let path = secret.get_path().unwrap();
            println!("{}", path);
        }
        Commands::Account(CmdAccount::LoginUrl) => {
            let api = secret.create_api().await?;
            let session = api.login_guest().await?;
            println!("https://playit.gg/login/guest-account/{}", session.session_key)
        }
        Commands::Claim(CmdClaim::Generate) => {
            ui.write_screen(claim_generate()).await;
        }
        Commands::Claim(CmdClaim::Url(command)) => {
            /* TODO: add agent type and name to URL */
            ui.write_screen(format!(
                "https://playit.gg/claim/{}",
                command.claim_code,
            )).await;
        }
        Commands::Claim(CmdClaim::Exchange(command)) => {
            let agent_type = match command.agent_type {
                CmdAgentType::Asignable => AgentType::Assignable,
                CmdAgentType::SelfManaged => AgentType::SelfManaged,
            };

            let secret_key = claim_exchange(&mut ui, &command.claim_code, agent_type, command.wait).await?;
            ui.write_screen(secret_key).await;
        }
        Commands::Tunnels(CmdTunnels::Prepare(command)) => {
            let api = secret.create_api().await?;

            let (tunnel_type, port_type) = match command.tunnel_type {
                CmdTunnelType::Both => (None, PortType::Both),
                CmdTunnelType::Tcp => (None, PortType::Tcp),
                CmdTunnelType::Udp => (None, PortType::Udp),
                CmdTunnelType::MinecraftJava => (Some(serde_json::to_string(&TunnelType::MinecraftJava).unwrap()), PortType::Tcp),
                CmdTunnelType::MinecraftBedrock => (Some(serde_json::to_string(&TunnelType::MinecraftBedrock).unwrap()), PortType::Tcp),
            };

            let target_region_num = match command.region {
                CmdTunnelRegion::GlobalAnycast => PlayitRegion::Global,
                CmdTunnelRegion::Optimal => {
                    let region = api.query_region(ReqQueryRegion { limit_region: None }).await?;
                    match region.region {
                        playit_api_client::api::PlayitRegion::GlobalAnycast => PlayitRegion::Global,
                        playit_api_client::api::PlayitRegion::Asia => PlayitRegion::Asia,
                        playit_api_client::api::PlayitRegion::NorthAmerica => PlayitRegion::NorthAmerica,
                        playit_api_client::api::PlayitRegion::Europe => PlayitRegion::Europe,
                        playit_api_client::api::PlayitRegion::India => PlayitRegion::India,
                        playit_api_client::api::PlayitRegion::SouthAmerica => PlayitRegion::SouthAmerica,
                    }
                }
                CmdTunnelRegion::NorthAmerica => PlayitRegion::NorthAmerica,
                CmdTunnelRegion::Europe => PlayitRegion::Europe,
                CmdTunnelRegion::Asia => PlayitRegion::Asia,
                CmdTunnelRegion::India => PlayitRegion::India,
                CmdTunnelRegion::SouthAmerica => PlayitRegion::SouthAmerica,
            } as u16;

            let existing = 'find_update: {
                if command.create_new {
                    break 'find_update None;
                }

                let data = api.agents_rundata().await?;
                if data.account_status != AgentAccountStatus::Ready {
                    println!("Invalid account status: {:?}", data.account_status);
                    return Ok(std::process::ExitCode::FAILURE);
                }

                for tunnel in data.tunnels {
                    let port_count = tunnel.port.to - tunnel.port.from;
                    if tunnel.tunnel_type != tunnel_type {
                        continue;
                    }
                    if command.port_count != port_count {
                        continue;
                    }
                    if !command.ignore_name && tunnel.name.as_ref() != Some(&command.name) {
                        continue;
                    }
                    if tunnel.proto != port_type {
                        continue;
                    }

                    if !command.ignore_region && tunnel.region_num != target_region_num {
                        continue;
                    }

                    break 'find_update Some((tunnel.id, (tunnel.local_ip, tunnel.local_port)));
                }

                for tunnel in data.pending {
                    if tunnel.tunnel_type != tunnel_type {
                        continue;
                    }

                    if tunnel.proto != port_type {
                        continue;
                    }

                    if !command.ignore_name && tunnel.name.as_ref() != Some(&command.name) {
                        continue;
                    }

                    if !command.ignore_region && tunnel.region_num != target_region_num {
                        continue;
                    }
                }

                None
            };

            
        }
        Commands::Tunnels(CmdTunnels::Delete(command)) => {
            todo!()
        }
        Commands::Tunnels(CmdTunnels::List) => {
            todo!()
        }
        Commands::Tunnels(CmdTunnels::Find(command)) => {
            todo!()
        }
        Commands::Tunnels(CmdTunnels::WaitFor(command)) => {
            todo!()
        }
        Commands::Tunnels(CmdTunnels::Set(command)) => {
            todo!()
        }
    }

    Ok(std::process::ExitCode::SUCCESS)
}

pub fn claim_generate() -> String {
    let mut buffer = [0u8; 5];
    rand::thread_rng().fill(&mut buffer);
    hex::encode(&buffer)
}

pub fn claim_url(code: &str) -> Result<String, CliError> {
    if hex::decode(code).is_err() {
        return Err(CliError::InvalidClaimCode.into());
    }

    Ok(format!(
        "https://playit.gg/claim/{}",
        code,
    ))
}

pub async fn claim_exchange(ui: &mut UI, claim_code: &str, agent_type: AgentType, wait_sec: u32) -> Result<String, CliError> {
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
            let setup_res = api.claim_setup(ReqClaimSetup {
                code: claim_code.to_string(),
                agent_type,
                version: format!("playit-cli {}", env!("CARGO_PKG_VERSION")),
            }).await;

            let setup = match setup_res {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "Failed loading claim setup");
                    ui.write_screen(format!("{}\n\nError: {:?}", last_message, error)).await;
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
                    ui.write_screen("Program approved :). Secret code being setup.").await;
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
        match api.claim_exchange(ReqClaimExchange { code: claim_code.to_string() }).await {
            Ok(res) => break res.secret_key,
            Err(ApiError::Fail(status)) => {
                let msg = format!("code \"{}\" not ready, {:?}", claim_code, status);
                ui.write_screen(msg).await;
            }
            Err(error) => return Err(error.into()),
        };

        if now_milli() > end_at {
            ui.write_screen("you took too long to approve the program, closing").await;
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

impl Error for CliError {

}

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
            ApiError::Fail(fail) => CliError::ApiFail(serde_json::to_string(&fail).unwrap())
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
