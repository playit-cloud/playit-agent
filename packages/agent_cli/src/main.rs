use std::cmp::Ordering;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::str::FromStr;
use std::time::{Duration, Instant};

use args::{CliArgs, CliInterface, CmdAccount, CmdAgentType, CmdClaim, CmdTunnelProxyProtocol, CmdTunnelRegion, CmdTunnelType, CmdTunnels, Commands};
use clap::Parser;
use cli_io::{AttentionNeeded, ClaimSetupStatus, TunnelDetail, TunnelDetails};
use playit_agent_core::agent_control::platform::get_platform;
use playit_agent_core::agent_control::version::{get_version, register_version};
use playit_api_client::ip_resource::PlayitRegion;
use rand::Rng;
use serde::Serialize;
use uuid::Uuid;

use autorun::autorun;
use playit_api_client::{api::*, PlayitApi};
use playit_api_client::http_client::HttpClientError;
use playit_agent_core::agent_control::errors::SetupError;
use playit_secret::PlayitSecret;

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
pub mod cli_io;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run_main().await {
        Ok(res) => res,
        Err(error) => {
            tracing::error!(?error, "main failed");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run_main() -> Result<std::process::ExitCode, CliError> {
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

    let cmd = args.cmd.unwrap_or(Commands::Start);

    let log_only = args.stdout;
    let log_path = args.log_path;

    /* setup logging */
    let _guard = match (log_only, log_path, args.iface) {
        (true, Some(_), _) => panic!("try to use -s and -l at the same time"),
        /* append logs to file */
        (false, Some(path), _) => {
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
        /* write to stdout unless we're running as start */
        (true, None, _) => {
            let (non_blocking, guard) = if let Commands::Start = &cmd {
                tracing_appender::non_blocking(std::io::stdout())
            } else {
                tracing_appender::non_blocking(std::io::stderr())
            };
            
            tracing_subscriber::fmt()
                .with_ansi(get_platform() == Platform::Linux)
                .with_writer(non_blocking)
                .init();

            Some(guard)
        }
        /* write logs to stderror so caller can parse stdout */
        (false, None, CliInterface::Csv | CliInterface::Json) => {
            let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stderr());
            
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
        cli_interface: args.iface,
    });

    match cmd {
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
            println!("{}", claim_generate());
        }
        Commands::Claim(CmdClaim::Url(command)) => {
            let Ok(code_bytes) = hex::decode(command.claim_code.trim()) else {
                ui.write_error("invalid claim code", "NotHex");
                return Ok(ExitCode::FAILURE);
            };

            if let Some(agent_name) = command.agent_name {
                println!("https://playit.gg/claim/{}?name={}", hex::encode(&code_bytes), urlencoding::encode(&agent_name));
            } else {
                println!("https://playit.gg/claim/{}", hex::encode(&code_bytes));
            }
        }
        Commands::Claim(CmdClaim::Setup(command)) => {
            if hex::decode(command.claim_code.trim()).is_err() {
                ui.write_error("invalid claim code", "NotHex");
                return Ok(ExitCode::FAILURE);
            };

            let agent_type = match command.agent_type {
                CmdAgentType::Asignable => AgentType::Assignable,
                CmdAgentType::SelfManaged => AgentType::SelfManaged,
            };

            let res = claim_exchange(
                &mut ui,
                &command.claim_code,
                agent_type,
                command.wait
            ).await;

            let secret = ui.ok_or_fatal(res, "failed to setup secret");


            let api = PlayitApi::create(API_BASE.to_string(), Some(secret.clone()));
            ui.ok_or_fatal(api.proto_register(ReqProtoRegister {
                agent_version: get_version(),
                client_addr: "127.0.0.1:100".parse().unwrap(),
                tunnel_addr: "127.0.0.1:200".parse().unwrap(),
            }).await, "failed to register program");

            println!("{}", secret);

            return Ok(ExitCode::SUCCESS);
        }
        Commands::Claim(CmdClaim::Exchange(command)) => {
            if hex::decode(command.claim_code.trim()).is_err() {
                ui.write_error("invalid claim code", "NotHex");
                return Ok(ExitCode::FAILURE);
            };

            let api = PlayitApi::create(API_BASE.to_string(), None);

            let secret_key = match api.claim_exchange(ReqClaimExchange { code: command.claim_code }).await {
                Ok(res) => res.secret_key,
                Err(ApiError::Fail(error)) => {
                    return Err(CliError::ClaimExchangeError(error));
                }
                Err(error) => return Err(error.into()),
            };

            println!("{}", secret_key);
        }
        Commands::Tunnels(CmdTunnels::Prepare(command)) => {
            let api = ui.ok_or_fatal(
                secret.create_api().await, 
                "failed to setup api client"
            );

            let local_address = match IpAddr::from_str(&command.local_address) {
                Ok(ip) => (ip, 0),
                Err(_) => match SocketAddr::from_str(&command.local_address) {
                    Ok(addr) => (addr.ip(), addr.port()),
                    Err(_) => {
                        ui.write_error("failed to parse local address", "InvalidSocketAddr");
                        return Ok(ExitCode::FAILURE);
                    }
                }
            };

            ui.write_status("loading agent data").await;

            let mut agent_data = ui.ok_or_fatal(
                api.agents_rundata().await,
                "failed to get agent data"
            );

            tracing::info!(
                agent_id = %agent_data.agent_id,
                agent_type = ?agent_data.agent_type,
                agent_status = ?agent_data.account_status,
                tunnel_count = agent_data.tunnels.len(),
                pending_tunnel_count = agent_data.pending.len(),
                "got agent data"
            );

            /* TODO: check agent status and error if invalid */
            // if agent_data.account_status != AgentAccountStatus::Ready {
            //     return Ok(std::process::ExitCode::FAILURE);
            // }

            let target_region = match command.region {
                CmdTunnelRegion::GlobalAnycast => PlayitRegion::Global,
                CmdTunnelRegion::Optimal => {
                    if agent_data.account_features.regional_tunnels {
                        match api.query_region(ReqQueryRegion { limit_region: None }).await {
                            Ok(data) => match data.region {   
                                playit_api_client::api::PlayitRegion::GlobalAnycast => PlayitRegion::Global,
                                playit_api_client::api::PlayitRegion::Asia => PlayitRegion::Asia,
                                playit_api_client::api::PlayitRegion::NorthAmerica => PlayitRegion::NorthAmerica,
                                playit_api_client::api::PlayitRegion::Europe => PlayitRegion::Europe,
                                playit_api_client::api::PlayitRegion::India => PlayitRegion::India,
                                playit_api_client::api::PlayitRegion::SouthAmerica => PlayitRegion::SouthAmerica,
                            },
                            Err(error) => {
                                ui.write_error("failed to determine optional region, using global", error);
                                PlayitRegion::Global
                            }
                        }
                    } else {
                        PlayitRegion::Global
                    }
                }
                CmdTunnelRegion::NorthAmerica => PlayitRegion::NorthAmerica,
                CmdTunnelRegion::Europe => PlayitRegion::Europe,
                CmdTunnelRegion::Asia => PlayitRegion::Asia,
                CmdTunnelRegion::India => PlayitRegion::India,
                CmdTunnelRegion::SouthAmerica => PlayitRegion::SouthAmerica,
            };

            tracing::info!("determined tunnel target region");

            let target_region_num = target_region as u16;

            let target_alloc = if let Some(ded_ip) = command.use_dedicated_ip {
                TunnelCreateUseAllocation::DedicatedIp(UseAllocDedicatedIp {
                    ip_hostname: ded_ip,
                    port: command.public_port,
                })
            } else {
                TunnelCreateUseAllocation::Region(UseRegion {
                    region: match target_region {
                        PlayitRegion::Anycast => AllocationRegion::Global,
                        PlayitRegion::Global => AllocationRegion::Global,
                        PlayitRegion::NorthAmerica => AllocationRegion::NorthAmerica,
                        PlayitRegion::Europe => AllocationRegion::Europe,
                        PlayitRegion::Asia => AllocationRegion::Asia,
                        PlayitRegion::India => AllocationRegion::India,
                        PlayitRegion::SouthAmerica => AllocationRegion::SouthAmerica,
                    },
                })
            };

            let (tunnel_type, port_type) = match command.tunnel_type {
                CmdTunnelType::Both => (None, PortType::Both),
                CmdTunnelType::Tcp => (None, PortType::Tcp),
                CmdTunnelType::Udp => (None, PortType::Udp),
                CmdTunnelType::MinecraftJava => (Some(TunnelType::MinecraftJava), PortType::Tcp),
                CmdTunnelType::MinecraftBedrock => (Some(TunnelType::MinecraftBedrock), PortType::Tcp),
            };

            let proxy_protocol = command.proxy_protocol.and_then(|proto| match proto {
                CmdTunnelProxyProtocol::None => None,
                CmdTunnelProxyProtocol::ProxyProtocolV1 => Some(ProxyProtocol::ProxyProtocolV1),
                CmdTunnelProxyProtocol::ProxyProtocolV2 => Some(ProxyProtocol::ProxyProtocolV2),
            });

            let existing = 'find_update: {
                if command.create_new == Some(true) {
                    tracing::info!("create new so do not search for existig tunnel");
                    break 'find_update None;
                }

                tracing::info!("searching for existing tunnel that matches arguments");

                /* put tunnels with matching name first */
                {
                    agent_data.tunnels.sort_by(|a, b| {
                        let a_name = a.name.as_ref() == Some(&command.name);
                        let b_name = b.name.as_ref() == Some(&command.name);

                        if a_name == b_name {
                            Ordering::Equal
                        } else if a_name {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    });

                    agent_data.pending.sort_by(|a, b| {
                        let a_name = a.name.as_ref() == Some(&command.name);
                        let b_name = b.name.as_ref() == Some(&command.name);

                        if a_name == b_name {
                            Ordering::Equal
                        } else if a_name {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    });
                }

                for tunnel in agent_data.tunnels {
                    let port_count = tunnel.port.to - tunnel.port.from;
                    if command.port_count != port_count {
                        continue;
                    }
                    if command.require_name && tunnel.name.as_ref() != Some(&command.name) {
                        continue;
                    }
                    if tunnel.proto != port_type {
                        continue;
                    }
                    if command.require_region && tunnel.region_num != target_region_num {
                        continue;
                    }

                    let actual_type = tunnel.tunnel_type.as_ref()
                        .and_then(|s| serde_json::from_str::<TunnelType>(&format!("{:?}", s)).ok());

                    if actual_type != tunnel_type {
                        continue;
                    }

                    tracing::info!(tunnel_id = %tunnel.id, "Found existing tunnel");
                    break 'find_update Some(TunnelOrPending::Tunnel(tunnel));
                }

                for tunnel in agent_data.pending {
                    if tunnel.proto != port_type {
                        continue;
                    }

                    if !command.require_name && tunnel.name.as_ref() != Some(&command.name) {
                        continue;
                    }

                    if command.require_region && tunnel.region_num != target_region_num {
                        continue;
                    }

                    let actual_type = tunnel.tunnel_type.as_ref()
                        .and_then(|s| serde_json::from_str::<TunnelType>(&format!("{:?}", s)).ok());

                    if actual_type != tunnel_type {
                        continue;
                    }

                    tracing::info!(tunnel_id = %tunnel.id, "Found pending tunnel");
                    break 'find_update Some(TunnelOrPending::Pending(tunnel));
                }

                None
            };

            let existing = match existing {
                None => None,
                Some(TunnelOrPending::Tunnel(tunn)) => Some(tunn),
                Some(TunnelOrPending::Pending(pending)) => {
                    if pending.is_disabled {
                        tracing::info!("pending is disabled enable");
    
                        ui.write_status("enable pending tunnel").await;
    
                        let update_res = api.tunnels_update(ReqTunnelsUpdate {
                            tunnel_id: pending.id,
                            local_ip: local_address.0,
                            local_port: if local_address.1 == 0 { None } else { Some(local_address.1) },
                            agent_id: Some(agent_data.agent_id),
                            enabled: true,
                        }).await;
    
                        ui.ok_or_fatal(update_res, "failed to enable pending tunnel");
                    }

                    tracing::info!("wait for pending tunnel");
    
                    let wait_till: Option<Instant> = if command.wait == 0 {
                        None
                    } else {
                        Some(Instant::now() + Duration::from_secs(command.wait as _))
                    };
    
                    loop {
                        if wait_till.map(|wait_till| wait_till < Instant::now()).unwrap_or(false) {
                            ui.write_error("timeout waiting for pending tunnel", "PendingTunnelTimeout");
                            return Ok(ExitCode::FAILURE);
                        }

                        ui.write_status("waiting 2s before checking tunnel status").await;
                        tokio::time::sleep(Duration::from_secs(2)).await;
    
                        ui.write_status("checking tunnel status").await;
    
                        let data = ui.ok_or_fatal(
                            api.agents_rundata().await, 
                            "failed to load agent data"
                        );
    
                        if let Some(tunnel) = data.tunnels.into_iter().find(|v| v.id == pending.id) {
                            ui.write_status("tunnel created").await;
                            break Some(tunnel);
                        }

                        if let Some(pending) = data.pending.into_iter().find(|v| v.id == pending.id) {
                            if pending.is_disabled {
                                ui.write_error("pending tunnel turned disabled", "PendingTunnelDisabled");
                                ui.write_message(AttentionNeeded {
                                    note: "Pending tunnel is disabled, please visit url:".to_string(),
                                    url: format!("https://playit.gg/account/tunnels/{}", pending.id),
                                }).await;

                                return Ok(ExitCode::FAILURE);
                            }

                            tracing::info!(tunnel_id = %pending.id, "tunnel still pending");
                        } else {
                            ui.write_error("pending tunnel deleted", "PendingTunnelDeleted");
                            return Ok(ExitCode::FAILURE);
                        }
                    }
                }
            };

            let tunnel = if let Some(tunnel) = existing {
                let needs_updated = 'set_needs_update: {
                    match tunnel.disabled {
                        Some(AgentTunnelDisabled::ByUser) => {
                            tracing::info!("tunnel is disabled by user, needs to be enabled");
                            break 'set_needs_update true;
                        },
                        Some(AgentTunnelDisabled::BySystem) => {
                            ui.write_error("tunnel is disabled", "DisabledBySystem");
                            return Ok(ExitCode::FAILURE);
                        }
                        None => {}
                    };

                    if tunnel.local_ip != local_address.0 || tunnel.local_port != local_address.1 {
                        tracing::info!("local address different than desired");
                        break 'set_needs_update true;
                    }

                    false
                };

                if needs_updated {                    
                    ui.write_status("updating tunnel").await;

                    let update_res = api.tunnels_update(ReqTunnelsUpdate {
                        tunnel_id: tunnel.id,
                        local_ip: local_address.0,
                        local_port: if local_address.1 == 0 { None } else { Some(local_address.1) },
                        agent_id: Some(agent_data.agent_id),
                        enabled: true,
                    }).await;

                    ui.ok_or_fatal(update_res, "failed to update tunnel");
                    tracing::info!("tunnel updated");
                }

                if proxy_protocol != tunnel.proxy_protocol {
                    tracing::info!(
                        current = ?tunnel.proxy_protocol,
                        target = ?proxy_protocol,
                        "proxy protocols needs updating"
                    );

                    ui.write_status("updating tunnel's proxy protocol").await;
                    ui.ok_or_fatal(
                        api.tunnels_proxy_set(ReqTunnelsProxySet {
                            tunnel_id: tunnel.id,
                            proxy_protocol,
                        }).await,
                        "failed to set proxy protocol"
                    );
                }

                /* TODO: check firewall instead of blindly setting */

                ui.write_status("tunnel ready").await;
                tunnel
            } else {
                if command.create_new == Some(false) {
                    println!("None");
                    return Ok(ExitCode::SUCCESS);
                }

                tracing::info!("could not find suitable tunnel, creating new tunnel");
                ui.write_status("creating new tunnel to meet requirements").await;

                let result = api.tunnels_create(ReqTunnelsCreate {
                    name: Some(command.name),
                    tunnel_type,
                    port_type,
                    port_count: command.port_count,
                    origin: TunnelOriginCreate::Agent(AssignedAgentCreate {
                        agent_id: agent_data.agent_id,
                        local_ip: local_address.0,
                        local_port: if local_address.1 == 0 { None } else { Some(local_address.1) },
                    }),
                    enabled: true,
                    alloc: Some(target_alloc),
                    firewall_id: command.firewall_id,
                    proxy_protocol,
                }).await;

                let tunnel_id = match result {
                    Ok(id) => id.id,
                    Err(error) => {
                        ui.write_error("failed to create tunnel", error);
                        return Ok(ExitCode::FAILURE)
                    }
                };

                tracing::info!("wait for pending tunnel");

                let wait_till: Option<Instant> = if command.wait == 0 {
                    None
                } else {
                    Some(Instant::now() + Duration::from_secs(command.wait as _))
                };

                loop {
                    if wait_till.map(|wait_till| wait_till < Instant::now()).unwrap_or(false) {
                        ui.write_error("timeout waiting for pending tunnel", "PendingTunnelTimeout");
                        return Ok(ExitCode::FAILURE);
                    }

                    ui.write_status("waiting 2s before checking tunnel status").await;
                    tokio::time::sleep(Duration::from_secs(2)).await;

                    ui.write_status("checking tunnel status").await;

                    let data = ui.ok_or_fatal(
                        api.agents_rundata().await, 
                        "failed to load agent data"
                    );

                    if let Some(tunnel) = data.tunnels.into_iter().find(|v| v.id == tunnel_id) {
                        ui.write_status("tunnel created").await;
                        break tunnel;
                    }

                    if let Some(pending) = data.pending.into_iter().find(|v| v.id == tunnel_id) {
                        if pending.is_disabled {
                            ui.write_error("pending tunnel turned disabled", "PendingTunnelDisabled");
                            return Ok(ExitCode::FAILURE);
                        }

                        tracing::info!(%tunnel_id, "tunnel still pending");
                    } else {
                        ui.write_error("pending tunnel deleted", "PendingTunnelDeleted");
                        return Ok(ExitCode::FAILURE);
                    }
                }
            };

            ui.write_message(TunnelDetails {
                detail: TunnelDetail::Id,
                value: tunnel.id.to_string(),
            }).await;

            ui.write_message(TunnelDetails {
                detail: TunnelDetail::ManageUrl,
                value: format!("https://playit.gg/account/tunnels/{}", tunnel.id),
            }).await;

            let domain = tunnel.custom_domain.unwrap_or(tunnel.assigned_domain);

            ui.write_message(TunnelDetails {
                detail: TunnelDetail::Address,
                value: format!("{}:{}", domain, tunnel.port.from),
            }).await;

            ui.write_message(TunnelDetails {
                detail: TunnelDetail::Domain,
                value: domain,
            }).await;

            ui.write_message(TunnelDetails {
                detail: TunnelDetail::PortStart,
                value: tunnel.port.from.to_string(),
            }).await;

            ui.write_message(TunnelDetails {
                detail: TunnelDetail::Region,
                value: PlayitRegion::from_num(tunnel.region_num)
                    .map(|v| format!("{:?}", v))
                    .unwrap_or_else(|| "Unknown".to_string()),
            }).await;

            println!("{}", tunnel.id);
        }
        // Commands::Tunnels(CmdTunnels::Delete(command)) => {
        //     todo!()
        // }
        // Commands::Tunnels(CmdTunnels::List) => {
        //     todo!()
        // }
        // Commands::Tunnels(CmdTunnels::Find(command)) => {
        //     todo!()
        // }
        // Commands::Tunnels(CmdTunnels::WaitFor(command)) => {
        //     todo!()
        // }
        // Commands::Tunnels(CmdTunnels::Set(command)) => {
        //     todo!()
        // }
    }

    Ok(std::process::ExitCode::SUCCESS)
}

pub fn claim_generate() -> String {
    let mut buffer = [0u8; 5];
    rand::thread_rng().fill(&mut buffer);
    hex::encode(&buffer)
}

pub async fn claim_exchange(ui: &mut UI, claim_code: &str, agent_type: AgentType, wait_sec: u32) -> Result<String, CliError> {
    let api = PlayitApi::create(API_BASE.to_string(), None);

    let end_at = if wait_sec == 0 {
        None
    } else {
        Some(Instant::now() + Duration::from_secs(wait_sec as _))
    };

    {
        let _close_guard = get_signal_handle().close_guard();

        while end_at.map(|end_at| Instant::now() < end_at).unwrap_or(true) {
            let setup_res = api.claim_setup(ReqClaimSetup {
                code: claim_code.to_string(),
                agent_type,
                version: format!("playit-cli {}", env!("CARGO_PKG_VERSION")),
            }).await;

            let setup = match setup_res {
                Ok(response) => response,
                Err(error) => {
                    ui.write_error("Failed to load claim setup", error);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            ui.write_message(ClaimSetupStatus {
                status: setup,
                claim_url: format!("https://playit.gg/claim/{}", claim_code),
            }).await;

            match setup {
                ClaimSetupResponse::UserAccepted => break,
                ClaimSetupResponse::UserRejected => {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    return Err(CliError::AgentClaimRejected);
                }
                _ => {}
            };

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    let secret_key = match api.claim_exchange(ReqClaimExchange { code: claim_code.to_string() }).await {
        Ok(res) => res.secret_key,
        Err(ApiError::Fail(error)) => {
            ui.write_error("exchange failed", error);
            return Err(CliError::ClaimExchangeError(error));
        }
        Err(error) => return Err(error.into()),
    };

    Ok(secret_key)
}

enum TunnelOrPending {
    Tunnel(AgentTunnel),
    Pending(AgentPendingTunnel),
}

#[derive(Debug)]
pub enum CliError {
    InvalidClaimCode,
    ClaimExchangeError(ClaimExchangeError),
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

impl Serialize for CliError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: serde::Serializer {
        serializer.serialize_str(&format!("{:?}", self))
    }
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
