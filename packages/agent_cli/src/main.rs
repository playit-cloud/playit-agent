use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};
use std::process::Termination;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use clap::{arg, ArgMatches, Command};
use rand::Rng;
use tokio::io::AsyncBufReadExt;
use uuid::Uuid;

use playit_agent_core::api::api::{AccountTunnel, AccountTunnelAllocation, AgentType, ApiError, ApiErrorNoFail, ApiResponseError, AssignedManagedCreate, ClaimExchangeError, ClaimSetupResponse, PortType, ReqClaimExchange, ReqClaimSetup, ReqTunnelsCreate, ReqTunnelsList, TunnelAllocated, TunnelOriginCreate, TunnelType};
use playit_agent_core::api::http_client::HttpClientError;
use playit_agent_core::api::ip_resource::IpResource;
use playit_agent_core::api::PlayitApi;
use playit_agent_core::network::address_lookup::{AddressLookup, AddressValue};
use playit_agent_core::tunnel_runner::TunnelRunner;
use playit_agent_core::utils::now_milli;
use playit_agent_proto::PortProto;

use crate::launch::{launch, LaunchConfig};
use crate::util::load_config;

pub const API_BASE: &'static str = "https://api.playit.gg";

pub mod launch;
pub mod util;

#[tokio::main]
async fn main() -> Result<std::process::ExitCode, anyhow::Error> {
    let matches = cli().get_matches();

    let secret = Secrets::load(&matches).await;

    match matches.subcommand() {
        Some(("version", _)) => println!("{}", env!("CARGO_PKG_VERSION")),
        Some(("account", m)) => match m.subcommand() {
            Some(("login-url", _)) => {
                let api = PlayitApi::create(API_BASE.to_string(), Some(secret.get()?));
                let session = api.login_create_guest().await?;
                println!("https://playit.gg/login/guest-account/{}", session.session_key)
            }
            Some(("status", _)) => {
                let api = PlayitApi::create(API_BASE.to_string(), Some(secret.get()?));
                println!("not implemented");

                // let res = api.req(GetSession).await?;
                // println!("ACCOUNT_ID={}", res.account_id);
                // println!("IS_GUEST={}", res.is_guest);
                // println!("EMAIL_VERIFIED={}", res.email_verified);
                // if let Some(agent_id) = res.agent_id {
                //     println!("AGENT_ID={}", agent_id);
                // }
                // println!("HAS_NOTICE={}", res.notice.is_some());
            }
            Some(("notice", _)) => {
                let api = PlayitApi::create(API_BASE.to_string(), Some(secret.get()?));
                println!("not implemented");

                // let res = api.req(GetSession).await?;
                // match res.notice {
                //     Some(notice) => println!("{}\n{}", notice.url, notice.message),
                //     None => println!("NONE"),
                // }
            }
            _ => return Err(CliError::NotImplemented.into()),
        }
        Some(("claim", m)) => match m.subcommand() {
            Some(("generate", _)) => {
                println!("{}", claim_generate());
            }
            Some(("url", m)) => {
                let code = m.get_one::<String>("CLAIM_CODE").expect("required");
                println!("{}", claim_url(code)?);
            }
            Some(("exchange", m)) => {
                let claim_code = m.get_one::<String>("CLAIM_CODE").expect("required");
                let wait: u32 = m.get_one::<String>("wait").expect("required").parse().expect("invalid wait value");

                let secret_key = match claim_exchange(claim_code, wait).await? {
                    Some(v) => v,
                    None => {
                        eprintln!("reached time limit");
                        return Ok(std::process::ExitCode::FAILURE);
                    }
                };

                println!("{}", secret_key);
            }
            _ => return Err(CliError::NotImplemented.into()),
        },
        Some(("tunnels", m)) => match m.subcommand() {
            Some(("prepare", m)) => {
                let api = PlayitApi::create(API_BASE.to_string(), Some(secret.get()?));

                let name = m.get_one::<String>("NAME").cloned();
                let tunnel_type: Option<TunnelType> = m.get_one::<String>("TUNNEL_TYPE")
                    .and_then(|v| serde_json::from_str(&format!("{:?}", v)).ok());
                let port_type = serde_json::from_str::<PortType>(&format!("{:?}", m.get_one::<String>("PORT_TYPE").expect("required")))
                    .map_err(|_| CliError::InvalidPortType)?;
                let port_count = m.get_one::<String>("PORT_COUNT").expect("required")
                    .parse::<u16>().map_err(|_| CliError::InvalidPortCount)?;
                let exact = m.get_flag("exact");
                let ignore_name = m.get_flag("ignore_name");

                let tunnel = tunnels_prepare(
                    &api, name, tunnel_type, port_type,
                    port_count, exact, ignore_name,
                ).await?;

                println!("{}", tunnel.id);
            }
            Some(("list", _)) => {
                let api = PlayitApi::create(API_BASE.to_string(), Some(secret.get()?));
                let tunnels = api.tunnels_list(ReqTunnelsList { tunnel_id: None, agent_id: None }).await?;
                for tunnel in tunnels.tunnels {
                    println!("{}", serde_json::to_string(&tunnel).unwrap());
                }
            }
            _ => return Err(CliError::NotImplemented.into())
        }
        Some(("run", m)) => {
            let _ = tracing_subscriber::fmt().try_init();

            let secret_key = secret.get()?;
            let api = PlayitApi::create(API_BASE.to_string(), Some(secret_key.clone()));
            let tunnels = api.tunnels_list(ReqTunnelsList { tunnel_id: None, agent_id: None }).await?;
            let mut tunnel_lookup = HashMap::new();
            let mut tunnel_found = HashSet::new();

            for tunnel in tunnels.tunnels {
                tunnel_found.insert(tunnel.id);
                tunnel_lookup.insert(tunnel.id, tunnel);
            }

            let mapping_override_strings: Vec<String> = match m.get_many::<String>("MAPPING_OVERRIDE") {
                Some(v) => v.into_iter().map(|v| v.to_string()).collect(),
                None => vec![],
            };

            let mut mapping_overrides = Vec::new();
            for override_str in mapping_override_strings {
                let mut parts = override_str.split("=");

                let tunnel_id: Uuid = parts.next().ok_or(CliError::InvalidMappingOverride)?
                    .parse().map_err(|_| CliError::InvalidMappingOverride)?;

                let local_addr_str = parts.next().ok_or(CliError::InvalidMappingOverride)?;
                let local_addr = match SocketAddr::from_str(local_addr_str) {
                    Ok(addr) => addr,
                    _ => match u16::from_str(local_addr_str) {
                        Ok(port) => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
                        _ => return Err(CliError::InvalidMappingOverride.into()),
                    }
                };

                match tunnel_lookup.remove(&tunnel_id) {
                    Some(tunnel) => {
                        if let Some(over) = MappingOverride::new(tunnel, local_addr) {
                            mapping_overrides.push(over);
                        }
                    }
                    None => {
                        return if tunnel_found.contains(&tunnel_id) {
                            Err(CliError::TunnelOverwrittenAlready(tunnel_id).into())
                        } else {
                            Err(CliError::TunnelNotFound(tunnel_id).into())
                        };
                    }
                }
            }

            let tunnel = TunnelRunner::new(secret_key, Arc::new(LookupWithOverrides(mapping_overrides))).await?;
            tunnel.run().await;
        }
        Some(("launch", m)) => {
            let config_file = m.get_one::<String>("CONFIG_FILE").unwrap();
            let config = match load_config::<LaunchConfig>(&config_file).await {
                Some(v) => v,
                None => {
                    return Err(CliError::InvalidConfigFile.into());
                }
            };

            let _ = tracing_subscriber::fmt().try_init();
            launch(config).await?;
        }
        _ => return Err(CliError::NotImplemented.into()),
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
        "https://new.playit.gg/claim/{}",
        code,
    ))
}

pub async fn claim_exchange(claim_code: &str, wait_sec: u32) -> Result<Option<String>, CliError> {
    let api = PlayitApi::create(API_BASE.to_string(), None);

    let end_at = if wait_sec == 0 {
        u64::MAX
    } else {
        now_milli() + (wait_sec as u64) * 1000
    };

    loop {
        let setup = api.claim_setup(ReqClaimSetup {
            code: claim_code.to_string(),
            agent_type: AgentType::SelfManaged,
            version: format!("playit-cli {}", env!("CARGO_PKG_VERSION")),
        }).await?;

        match setup {
            ClaimSetupResponse::WaitingForUserVisit => {
                eprintln!("Waiting for user to visit {}", claim_url(claim_code)?);
            }
            ClaimSetupResponse::WaitingForUser => {
                eprintln!("Waiting for user to approve");
            }
            ClaimSetupResponse::UserAccepted => {
                eprintln!("User accepted, exchanging code for secret");
                break;
            }
            ClaimSetupResponse::UserRejected => {
                eprintln!("User rejected");
                return Err(CliError::AgentClaimRejected);
            }
        }

        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    let secret_key = loop {
        match api.claim_exchange(ReqClaimExchange { code: claim_code.to_string() }).await {
            Ok(res) => break res.secret_key,
            Err(ApiError::Fail(status)) => {
                eprintln!("code \"{}\" not ready, {:?}", claim_code, status);
            }
            Err(error) => return Err(error.into()),
        };

        if now_milli() > end_at {
            eprintln!("reached time limit");
            return Ok(None);
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    };

    Ok(Some(secret_key))
}

pub async fn tunnels_prepare(api: &PlayitApi, name: Option<String>, tunnel_type: Option<TunnelType>, port_type: PortType, port_count: u16, exact: bool, ignore_name: bool) -> Result<AccountTunnel, CliError> {
    let tunnels = api.tunnels_list(ReqTunnelsList { tunnel_id: None, agent_id: None }).await?;

    let mut options = Vec::new();
    for tunnel in tunnels.tunnels {
        let tunnel_port_count = match &tunnel.alloc {
            AccountTunnelAllocation::Allocated(alloc) => alloc.port_end - alloc.port_start,
            _ => continue,
        };

        if exact {
            if (ignore_name || tunnel.name.eq(&name)) && tunnel.port_type == port_type && port_count == tunnel_port_count && tunnel.tunnel_type == tunnel_type {
                options.push(tunnel);
            } else {
                continue;
            }
        } else {
            if (tunnel.port_type == PortType::Both || tunnel.port_type == port_type) && port_count <= tunnel_port_count && tunnel.tunnel_type == tunnel_type {
                options.push(tunnel);
            }
        }
    }

    /* rank options by how much they match */
    options.sort_by_key(|option| {
        let mut points = 0;

        if ignore_name {
            if name.is_some() && option.name.eq(&name) {
                points += 1;
            }
        } else {
            if option.name.eq(&name) {
                points += 10;
            }
        }

        if option.port_type == port_type {
            points += 200;
        }

        if port_count == option.port_count {
            points += 100;
        } else {
            points += ((port_count as i32) - (option.port_count as i32)) * 10;
        }

        points += match option.alloc {
            AccountTunnelAllocation::Pending => -10,
            AccountTunnelAllocation::Disabled => -40,
            AccountTunnelAllocation::Allocated(_) => 0,
        };

        points
    });

    if let Some(found_tunnel) = options.pop() {
        return Ok(found_tunnel);
    }

    let created = api.tunnels_create(ReqTunnelsCreate {
        name,
        tunnel_type,
        port_type,
        port_count,
        origin: TunnelOriginCreate::Managed(AssignedManagedCreate { agent_id: None }),
        enabled: true,
        alloc: None,
        firewall_id: None,
    }).await?;

    let tunnels = api.tunnels_list(ReqTunnelsList { tunnel_id: None, agent_id: None }).await?;
    for tunnel in tunnels.tunnels {
        if tunnel.id == created.id {
            return Ok(tunnel);
        }
    }

    Err(CliError::ResourceNotFoundAfterCreate(created.id))
}

pub struct MappingOverride {
    tunnel: AccountTunnel,
    alloc: TunnelAllocated,
    ip_resource: IpResource,
    local_addr: SocketAddr,
}

impl MappingOverride {
    pub fn new(tunnel: AccountTunnel, local_addr: SocketAddr) -> Option<Self> {
        let alloc = match &tunnel.alloc {
            AccountTunnelAllocation::Allocated(alloc) => alloc.clone(),
            _ => return None,
        };

        let ip_resource = IpResource::from_ip(alloc.tunnel_ip);
        Some(MappingOverride {
            tunnel,
            alloc,
            ip_resource,
            local_addr,
        })
    }
}

pub struct LookupWithOverrides(Vec<MappingOverride>);

impl AddressLookup for LookupWithOverrides {
    type Value = SocketAddr;

    fn lookup(&self, ip: IpAddr, port: u16, proto: PortType) -> Option<AddressValue<SocketAddr>> {
        let resource = IpResource::from_ip(ip);

        for over in &self.0 {
            if over.tunnel.port_type == proto && over.ip_resource == resource {
                return Some(AddressValue {
                    value: over.local_addr,
                    from_port: over.alloc.port_start,
                    to_port: over.alloc.port_end,
                });
            }
        }

        Some(AddressValue {
            value: "127.0.0.1".parse().unwrap(),
            from_port: port,
            to_port: port + 1,
        })
    }
}

pub struct Secrets {
    secret: Option<String>,
    path: Option<String>,
}

impl Secrets {
    pub fn get(&self) -> Result<String, CliError> {
        match &self.secret {
            Some(v) => Ok(v.clone()),
            None => Err(CliError::MissingSecret),
        }
    }

    pub async fn load(matches: &ArgMatches) -> Self {
        let (secret, path) = match matches.get_one::<String>("secret") {
            Some(v) => (Some(v.clone()), matches.get_one::<String>("secret_path").cloned()),
            None => match matches.get_one::<String>("secret_path") {
                Some(path) => {
                    if let Ok(secret) = tokio::fs::read_to_string(path).await {
                        (Some(secret), Some(path.clone()))
                    } else {
                        (None, Some(path.clone()))
                    }
                }
                None => (None, None),
            }
        };

        Secrets {
            secret,
            path,
        }
    }
}

#[derive(Debug)]
pub enum CliError {
    InvalidClaimCode,
    NotImplemented,
    MissingSecret,
    InvalidPortType,
    InvalidPortCount,
    InvalidMappingOverride,
    AgentClaimRejected,
    InvalidConfigFile,
    TunnelNotFound(Uuid),
    TunnelOverwrittenAlready(Uuid),
    ResourceNotFoundAfterCreate(Uuid),
    RequestError(HttpClientError),
    ApiError(ApiResponseError),
    ApiFail(String),
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

impl Display for CliError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for CliError {}

fn cli() -> Command {
    Command::new("playit-cli")
        .arg(arg!(--secret <SECRET> "secret code for the agent").required(false))
        .arg(arg!(--secret_path <PATH> "path to file containing secret").required(false))
        .subcommand_required(true)
        .subcommand(Command::new("version"))
        .subcommand(
            Command::new("account")
                .subcommand_required(true)
                .subcommand(
                    Command::new("login-url")
                        .about("Generates a link to allow user to login")
                )
                .subcommand(
                    Command::new("status")
                        .about("Print account status")
                )
                .subcommand(
                    Command::new("notice")
                        .about("Print notice for account")
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
            Command::new("run")
                .about("Run the playit agent")
                .arg(arg!([MAPPING_OVERRIDE] "(format \"<tunnel-id>=[<local-ip>:]<local-port> [, ..]\")").required(false).value_delimiter(','))
        )
        .subcommand(
            Command::new("launch")
                .about("Launches the playit agent with a configuration file")
                .arg(arg!(<CONFIG_FILE> "configuration file").required(true))
        )
}

pub struct MatchIp {
    pub ip_number: u64,
    pub region_id: u16,
}

impl MatchIp {
    pub fn new(ip: Ipv6Addr) -> Self {
        let parts = ip.octets();

        /* 6 bytes /48 BGP Routing */
        /* 2 bytes for region id */
        let region_id = u16::from_be_bytes([parts[6], parts[7]]);

        /* 8 bytes for ip number */
        let ip_number = u64::from_be_bytes([
            parts[8],
            parts[9],
            parts[10],
            parts[11],
            parts[12],
            parts[13],
            parts[14],
            parts[15],
        ]);

        MatchIp {
            ip_number,
            region_id,
        }
    }

    fn region_number_v4(ip: Ipv4Addr) -> u16 {
        let octs = ip.octets();

        /* 209.25.140.0/22 (1 to 4) */
        if octs[0] == 209 && octs[1] == 25 && octs[2] >= 140 && octs[2] <= 143 {
            1u16 + (octs[2] - 140) as u16
        }
        /* 23.133.216.0/24 (5) */
        else if octs[0] == 23 && octs[1] == 133 && octs[2] == 216 {
            5u16
        }
        /* global IP */
        else {
            0
        }
    }

    pub fn matches(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ip) => {
                let octs = ip.octets();

                if octs[3] as u64 != self.ip_number {
                    return false;
                }

                if self.region_id == 0 {
                    return true;
                }

                self.region_id == Self::region_number_v4(ip)
            }
            IpAddr::V6(ip) => {
                let other = MatchIp::new(ip);
                if self.region_id == 0 {
                    self.ip_number == other.ip_number
                } else {
                    self.ip_number == other.ip_number && self.region_id == other.region_id
                }
            }
        }
    }
}