use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;
use std::time::Duration;

use clap::{arg, Command};
use playit_agent_core::agent_control::platform::get_platform;
use playit_agent_core::agent_control::version::register_version;
use playit_agent_core::PROTOCOL_VERSION;
use rand::Rng;
use uuid::Uuid;

use autorun::autorun;
use playit_api_client::{api::*, PlayitApi};
use playit_api_client::http_client::HttpClientError;
use playit_agent_core::agent_control::errors::SetupError;
use playit_agent_core::utils::now_milli;
use playit_secret::PlayitSecret;

use crate::signal_handle::get_signal_handle;
use crate::ui::{UI, UISettings};

pub static API_BASE: LazyLock<String> = LazyLock::new(|| {
    dotenv::var("API_BASE").unwrap_or("https://api.playit.gg".to_string())
});

pub mod util;
pub mod autorun;
pub mod playit_secret;
pub mod ui;
pub mod signal_handle;

#[tokio::main]
async fn main() -> Result<std::process::ExitCode, CliError> {
    let matches = cli().get_matches();

    /* register docker */
    {
        let platform = if matches.get_flag("platform_docker") {
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
            proto_version: PROTOCOL_VERSION,
        });
    }

    let mut secret = PlayitSecret::from_args(&matches).await;
    let _ = secret.with_default_path().await;

    let log_only = matches.get_flag("stdout");
    let log_path = matches.get_one::<String>("log_path");

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

    match matches.subcommand() {
        None => {
            ui.write_screen("no command provided, doing auto run").await;
            tokio::time::sleep(Duration::from_secs(1)).await;
            autorun(&mut ui, secret).await?;
        }
        Some(("start", _)) => {
            autorun(&mut ui, secret).await?;
        }
        Some(("version", _)) => println!("{}", env!("CARGO_PKG_VERSION")),
        #[cfg(target_os = "linux")]
        Some(("setup", _)) => {
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
        Some(("reset", _)) => {
            loop {
                let mut secerts = PlayitSecret::from_args(&matches).await;
                secerts.with_default_path().await;

                let path = secerts.get_path().unwrap();
                if !tokio::fs::try_exists(path).await.unwrap_or(false) {
                    break;
                }

                tokio::fs::remove_file(path).await.unwrap();
                println!("deleted secret at: {}", path);
            }
        }
        Some(("secret-path", _)) => {
            let mut secerts = PlayitSecret::from_args(&matches).await;
            secerts.with_default_path().await;
            let path = secerts.get_path().unwrap();
            println!("{}", path);
        }
        Some(("account", m)) => match m.subcommand() {
            Some(("login-url", _)) => {
                let api = secret.create_api().await?;
                let session = api.login_guest().await?;
                println!("https://playit.gg/login/guest-account/{}", session.session_key)
            }
            _ => return Err(CliError::NotImplemented),
        }
        Some(("claim", m)) => match m.subcommand() {
            Some(("generate", _)) => {
                ui.write_screen(claim_generate()).await;
            }
            Some(("url", m)) => {
                let code = m.get_one::<String>("CLAIM_CODE").expect("required");
                ui.write_screen(claim_url(code)?.to_string()).await;
            }
            Some(("exchange", m)) => {
                let claim_code = m.get_one::<String>("CLAIM_CODE").expect("required");
                let wait: u32 = m.get_one::<String>("wait").expect("required").parse().expect("invalid wait value");

                let secret_key = claim_exchange(&mut ui, claim_code, AgentType::SelfManaged, wait).await?;
                ui.write_screen(secret_key).await;
            }
            _ => return Err(CliError::NotImplemented),
        },
        Some(("tunnels", m)) => match m.subcommand() {
            Some(("prepare", m)) => {
                unimplemented!()
                // let api = secret.create_api().await?;

                // let name = m.get_one::<String>("NAME").cloned();
                // let tunnel_type: Option<TunnelType> = m.get_one::<String>("TUNNEL_TYPE")
                //     .and_then(|v| serde_json::from_str(&format!("{:?}", v)).ok());
                // let port_type = serde_json::from_str::<PortType>(&format!("{:?}", m.get_one::<String>("PORT_TYPE").expect("required")))
                //     .map_err(|_| CliError::InvalidPortType)?;
                // let port_count = m.get_one::<String>("PORT_COUNT").expect("required")
                //     .parse::<u16>().map_err(|_| CliError::InvalidPortCount)?;
                // let exact = m.get_flag("exact");
                // let ignore_name = m.get_flag("ignore_name");

                // let tunnel_id = tunnels_prepare(
                //     &api, name, tunnel_type, port_type,
                //     port_count, exact, ignore_name,
                // ).await?;

                // println!("{}", tunnel_id);
            }
            Some(("list", _)) => {
                let api = secret.create_api().await?;
                let response = api.tunnels_list_json(ReqTunnelsList { tunnel_id: None, agent_id: None }).await?;
                println!("{}", serde_json::to_string_pretty(&response).unwrap());
            }
            _ => return Err(CliError::NotImplemented)
        }
        _ => return Err(CliError::NotImplemented),
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
        return Err(CliError::InvalidClaimCode);
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

struct TunnelOption {
    id: Uuid,
    name: Option<String>,
    proto: PortType,
    port_count: u16,
    tunnel_type: Option<String>,
    public_address: Option<TunnelAlloc>,
}

struct TunnelAlloc {
    address: String,
    port: u16,
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

fn cli() -> Command {
    let mut cmd = Command::new("playit-cli")
        .arg(arg!(--secret <SECRET> "secret code for the agent").required(false))
        .arg(arg!(--secret_path <PATH> "path to file containing secret").required(false))
        .arg(arg!(-w --secret_wait "wait for secret_path file to read secret").required(false))
        .arg(arg!(-s --stdout "prints logs to stdout").required(false))
        .arg(arg!(-l --log_path <PATH> "path to write logs to").required(false))
        .arg(arg!(--platform_docker "overrides platform in version to be docker").required(false))
        .subcommand_required(false)
        .subcommand(Command::new("version"))
        .subcommand(
            Command::new("account")
                .subcommand_required(true)
                .subcommand(
                    Command::new("login-url")
                        .about("Generates a link to allow user to login")
                )
        )
        .subcommand(
            Command::new("claim")
                .subcommand_required(true)
                .arg(arg!(--name <TUNNEL_NAME> "name of the agent").required(false))
                .about("Setting up a new playit agent")
                .long_about("Provides a URL that can be visited to claim the agent and generate a secret key")
                .subcommand(
                    Command::new("generate")
                        .about("Generates a random claim code")
                )
                .subcommand(
                    Command::new("url")
                        .about("Print a claim URL given the code and options")
                        .arg(arg!(<CLAIM_CODE> "claim code"))
                        .arg(arg!(--name [NAME] "name for the agent").default_value("from-cli"))
                        .arg(arg!(--type [TYPE] "the agent type").default_value("self-managed"))
                )
                .subcommand(
                    Command::new("exchange")
                        .about("Exchanges the claim for the secret key")
                        .arg(arg!(<CLAIM_CODE> "claim code (see \"claim generate\")"))
                        .arg(arg!(--wait <WAIT_SEC> "number of seconds to wait 0=infinite").default_value("0"))
                )
        )
        .subcommand(
            Command::new("start")
                .about("Start the playit agent")
        )
        .subcommand(
            Command::new("tunnels")
                .subcommand_required(true)
                .about("Manage tunnels")
                .subcommand(
                    Command::new("prepare")
                        .about("Create a tunnel if it doesn't exist with the parameters")
                        .arg(arg!(--type [TUNNEL_TYPE] "the tunnel type"))
                        .arg(arg!(--name [NAME] "name of the tunnel"))
                        .arg(arg!(<PORT_TYPE> "either \"tcp\", \"udp\", or \"both\""))
                        .arg(arg!(<PORT_COUNT> "number of ports in a series to allocate").default_value("1"))
                        .arg(arg!(--exact))
                        .arg(arg!(--ignore_name))
                )
                .subcommand(
                    Command::new("list")
                        .about("List tunnels (format \"[tunnel-id] [port-type] [port-count] [public-address]\")")
                )
        )
        .subcommand(
            Command::new("reset")
                .about("removes the secret key on your system so the playit agent can be re-claimed")
        )
        .subcommand(
            Command::new("secret-path")
                .about("shows the file path where the playit secret can be found")
        )
        ;

    #[cfg(target_os = "linux")] {
        cmd = cmd.subcommand(Command::new("setup"));
    }

    cmd
}
