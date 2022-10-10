use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};
use std::process::Termination;
use std::sync::Arc;
use std::time::Duration;

use clap::{arg, ArgMatches, Command};
use rand::Rng;
use tokio::io::AsyncBufReadExt;
use uuid::Uuid;

use playit_agent_core::api::client::{ApiClient, ApiError};
use playit_agent_core::api::messages::{CreateTunnel, ListAccountTunnels, TunnelType};
use playit_agent_core::network::address_lookup::{AddressLookup, MatchAddress};
use playit_agent_core::tunnel_runner::TunnelRunner;
use playit_agent_core::utils::now_milli;
use playit_agent_proto::PortProto;

pub const API_BASE: &'static str = "https://api.playit.cloud";

#[tokio::main]
async fn main() -> Result<std::process::ExitCode, anyhow::Error> {
    let matches = cli().get_matches();

    let secret = Secrets::load(&matches).await;

    match matches.subcommand() {
        Some(("version", _)) => println!("{}", env!("CARGO_PKG_VERSION")),
        Some(("claim", m)) => match m.subcommand() {
            Some(("generate", _)) => {
                let mut buffer = [0u8; 5];
                rand::thread_rng().fill(&mut buffer);
                let claim_key = hex::encode(&buffer);
                println!("{}", claim_key);
            }
            Some(("url", m)) => {
                let code = m.get_one::<String>("CLAIM_CODE").expect("required");

                if hex::decode(code).is_err() {
                    return Err(CliError::InvalidClaimCode.into());
                }

                let name = m.get_one::<String>("name").expect("required");
                let agent_type = m.get_one::<String>("type").expect("required");

                println!("https://playit.gg/claim/{}?type={}&name={}", code, urlencoding::encode(agent_type), urlencoding::encode(name))
            }
            Some(("exchange", m)) => {
                let claim_code = m.get_one::<String>("CLAIM_CODE").expect("required");
                let wait: u32 = m.get_one::<String>("wait").expect("required").parse().expect("invalid wait value");

                let api = ApiClient::new(API_BASE.to_string(), None);

                let end_at = if wait == 0 {
                    u64::MAX
                } else {
                    now_milli() + (wait as u64) * 1000
                };

                let secret_key = loop {
                    match api.try_exchange_claim_for_secret(claim_code).await {
                        Ok(Some(value)) => break value,
                        Err(ApiError::HttpError(401, msg)) if msg.eq("your access has not been confirmed yet") => {
                            eprintln!("waiting for user approval with claim code \"{}\"", claim_code);
                        }
                        Ok(None) => {
                            eprintln!("code \"{}\" not claimed yet", claim_code);
                        }
                        Err(error) => return Err(error.into()),
                    };

                    if now_milli() > end_at {
                        eprintln!("reached time limit");
                        return Ok(std::process::ExitCode::FAILURE);
                    }

                    tokio::time::sleep(Duration::from_secs(2)).await;
                };

                println!("{}", secret_key);
            }
            _ => return Err(CliError::NotImplemented.into()),
        },
        Some(("tunnels", m)) => match m.subcommand() {
            Some(("prepare", m)) => {
                let api = ApiClient::new(API_BASE.to_string(), Some(secret.get()?));

                let name = m.get_one::<String>("NAME").cloned();
                let tunnel_type: Option<TunnelType> = m.get_one::<String>("TUNNEL_TYPE")
                    .and_then(|v| serde_json::from_str(&format!("{:?}", v)).ok());
                let port_type = serde_json::from_str::<PortProto>(&format!("{:?}", m.get_one::<String>("PORT_TYPE").expect("required")))
                    .map_err(|_| CliError::InvalidPortType)?;
                let port_count = m.get_one::<String>("PORT_COUNT").expect("required")
                    .parse::<u16>().map_err(|_| CliError::InvalidPortCount)?;
                let exact = m.get_flag("exact");
                let ignore_name = m.get_flag("ignore_name");

                let tunnels = api.req(ListAccountTunnels).await?;

                let mut options = Vec::new();
                for tunnel in tunnels.tunnels {
                    let tunnel_port_count = tunnel.to_port - tunnel.from_port;

                    if exact {
                        if (ignore_name || tunnel.name.eq(&name)) && tunnel.port_type == port_type && port_count == tunnel_port_count && tunnel.tunnel_type == tunnel_type {
                            options.push(tunnel);
                        } else {
                            continue;
                        }
                    } else {
                        if (tunnel.port_type == PortProto::Both || tunnel.port_type == port_type) && port_count <= tunnel_port_count && tunnel.tunnel_type == tunnel_type {
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

                    let tunnel_port_count = option.to_port - option.from_port;
                    if port_count == tunnel_port_count {
                        points += 100;
                    } else {
                        points += ((port_count as i32) - (tunnel_port_count as i32)) * 10;
                    }
                });

                if let Some(found_tunnel) = options.pop() {
                    println!("{}", found_tunnel.id);
                    return Ok(std::process::ExitCode::SUCCESS);
                }

                let created = api.req(CreateTunnel {
                    tunnel_type,
                    port_type,
                    port_count,
                    local_ip: "127.0.0.1".parse().unwrap(),
                    local_port: None,
                    agent_id: tunnels.agent_id,
                }).await?;

                println!("{}", created.id);
            }
            Some(("list", _)) => {
                let api = ApiClient::new(API_BASE.to_string(), Some(secret.get()?));
                let tunnels = api.req(ListAccountTunnels).await?;
                for tunnel in tunnels.tunnels {
                    println!(
                        "{} {} {} {}",
                        tunnel.id,
                        match tunnel.port_type {
                            PortProto::Both => "both",
                            PortProto::Tcp => "tcp",
                            PortProto::Udp => "udp",
                        },
                        tunnel.to_port - tunnel.from_port,
                        tunnel.display_address,
                    );
                }
            }
            _ => return Err(CliError::NotImplemented.into())
        }
        Some(("run", m)) => {
            let _ = tracing_subscriber::fmt().try_init();

            let secret_key = secret.get()?;
            let api = ApiClient::new(API_BASE.to_string(), Some(secret_key.clone()));
            let tunnels = api.req(ListAccountTunnels).await?;
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
                let local_port: u16 = parts.next().ok_or(CliError::InvalidMappingOverride)?
                    .parse().map_err(|_| CliError::InvalidMappingOverride)?;

                match tunnel_lookup.remove(&tunnel_id) {
                    Some(v) => {
                        mapping_overrides.push((v, local_port));
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

            let tunnel  = TunnelRunner::new(secret_key, Arc::new(SimpleLookup)).await?;
            tunnel.run().await;
        }
        _ => return Err(CliError::NotImplemented.into()),
    }

    Ok(std::process::ExitCode::SUCCESS)
}

pub struct SimpleLookup;

impl AddressLookup for SimpleLookup {
    fn find_tunnel_port_range(&self, match_ip: Ipv6Addr, port: u16) -> Option<(u16, u16)> {
        Some((port, port + 1))
    }

    fn local_address(&self, match_addr: MatchAddress, proto: PortProto) -> Option<SocketAddr> {
        Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, match_addr.from_port)))
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
    TunnelNotFound(Uuid),
    TunnelOverwrittenAlready(Uuid),
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
                .arg(arg!([MAPPING_OVERRIDE] "(format \"<tunnel-id>=<local-port> [, ..]\")").required(false).value_delimiter(','))
        )
}