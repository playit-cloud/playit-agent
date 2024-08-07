use std::{
    fmt::Write,
    net::{IpAddr, SocketAddr},
    sync::{Arc, atomic::Ordering, Mutex},
    time::Duration,
};

use playit_agent_core::{
    api::api::*,
    network::address_lookup::{AddressLookup, AddressValue},
    playit_agent::PlayitAgent,
    utils::now_milli,
};
use playit_agent_core::api::api::AgentType;

use crate::{API_BASE, CliError, match_ip::MatchIp, playit_secret::PlayitSecret, ui::UI};

pub async fn autorun(ui: &mut UI, mut secret: PlayitSecret) -> Result<(), CliError> {
    let secret_code = secret
        .ensure_valid(ui)
        .await?
        .get_or_setup(ui)
        .await?;

    let api = secret.create_api().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let lookup = {
        let data = api.agents_rundata().await?;
        let lookup = Arc::new(LocalLookup {
            data: Mutex::new(vec![]),
        });
        lookup.update(data.tunnels).await;

        lookup
    };

    let mut error_count = 0;
    ui.write_screen("starting up tunnel connection").await;

    let runner = loop {
        match PlayitAgent::new(API_BASE.to_string(), secret_code.clone(), lookup.clone()).await {
            Ok(res) => break res,
            Err(error) => {
                error_count += 1;
                if error_count > 5 {
                    ui.write_error("Final attempted failed to setup tunnel", &error).await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    return Err(CliError::TunnelSetupError(error));
                };

                ui.write_error("Failed to setup tunnel client", error).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };

    let signal = runner.keep_running();
    let runner = tokio::spawn(runner.run());

    ui.write_screen("tunnel running").await;

    let mut guest_login_link: Option<(String, u64)> = None;

    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;

        let account_tunnels_res = api.agents_rundata().await;
        let agent_data = match account_tunnels_res {
            Ok(v) => v,
            Err(error) => {
                ui.write_error("Failed to load latest tunnels", error).await;
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        let mut msg = format!(
            "playit (v{}): {} tunnel running, {} tunnels registered\n\n",
            env!("CARGO_PKG_VERSION"),
            now_milli(),
            agent_data.tunnels.len()
        );

        match agent_data.account_status {
            AgentAccountStatus::Guest => {
                'login_link: {
                    let now = now_milli();

                    match &guest_login_link {
                        Some((link, ts)) if now - *ts < 15_000 => {
                            writeln!(msg, "login: {}", link).unwrap();
                        }
                        _ => {
                            let Ok(session) = api.login_guest().await else {
                                writeln!(msg, "Failed to create guest login link").unwrap();
                                break 'login_link;
                            };

                            let link = format!("https://playit.gg/login/guest-account/{}", session.session_key);
                            writeln!(msg, "login: {}", link).unwrap();

                            guest_login_link = Some((link, now_milli()));
                        }
                    }
                }
            }
            AgentAccountStatus::EmailNotVerified => {
                writeln!(msg, "Email not verified https://playit.gg/account/settings/account/verify-email").unwrap();
            }
            AgentAccountStatus::AccountDeleteScheduled => {
                writeln!(msg, "Account scheduled for delete: https://playit.gg/account/settings/account/delete-account").unwrap();
            }
            AgentAccountStatus::Banned => {
                writeln!(msg, "Account banned: https://playit.gg/account").unwrap();
            }
            AgentAccountStatus::HasMessage => {
                writeln!(msg, "You have a message: https://playit.gg/account").unwrap();
            }
            AgentAccountStatus::AgentOverLimit => {
                writeln!(msg, "Too many agents: https://playit.gg/account/agents").unwrap();
            }
            AgentAccountStatus::AgentDisabled => {
                writeln!(msg, "Account disabled: https://playit.gg/account/agents/{}", agent_data.agent_id).unwrap();
            }
            AgentAccountStatus::Ready => {}
        }

        writeln!(msg, "\nTUNNELS").unwrap();

        if agent_data.tunnels.len() == 0 && agent_data.pending.len() == 0 {
            let agent_id = match agent_data.agent_type {
                AgentType::Default => "default".to_string(),
                AgentType::Assignable => agent_data.agent_id.to_string(),
                AgentType::SelfManaged => agent_data.agent_id.to_string(),
            };

            writeln!(msg, "Add tunnels here: https://playit.gg/account/agents/{}", agent_id).unwrap();
        } else {
            for tunnel in &agent_data.tunnels {
                let addr = tunnel.custom_domain.as_ref().unwrap_or(&tunnel.assigned_domain);
                let src = match tunnel.tunnel_type.as_ref().map(|v| v.as_str()) {
                    Some("minecraft-java") => addr.clone(),
                    _ => format!("{}:{}", addr, tunnel.port.from),
                };

                let dst = format!("{}:{}", tunnel.local_ip, tunnel.local_port);

                if let Some(disabled) = tunnel.disabled {
                    writeln!(msg, "{} => {} (disabled)", src, dst).unwrap();
                    if disabled == AgentTunnelDisabled::BySystem {
                        writeln!(msg, "\tsee: https://playit.gg/account/tunnels/{}", tunnel.id).unwrap();
                    }
                } else if let Some(tunnel_type) = &tunnel.tunnel_type {
                    writeln!(msg, "{} => {} ({})", src, dst, tunnel_type).unwrap();
                } else {
                    writeln!(msg, "{} => {} (proto: {:?}, port count: {})", src, dst, tunnel.proto, tunnel.port.to - tunnel.port.from).unwrap();
                }
            }

            for tunnel in &agent_data.pending {
                if tunnel.is_disabled {
                    writeln!(msg, "tunnel pending (disabled): https://playit.gg/account/tunnels/{}", tunnel.id).unwrap();
                } else {
                    writeln!(msg, "tunnel pending: https://playit.gg/account/tunnels/{}", tunnel.id).unwrap();
                }
            }
        }

        lookup.update(agent_data.tunnels).await;
        ui.write_screen(msg).await;
    }

    let _ = runner.await;
    signal.store(false, Ordering::SeqCst);

    Ok(())
}

pub struct LocalLookup {
    data: Mutex<Vec<TunnelEntry>>,
}

impl AddressLookup for LocalLookup {
    type Value = SocketAddr;

    fn lookup(&self, ip: IpAddr, port: u16, proto: PortType) -> Option<AddressValue<SocketAddr>> {
        let values = self.data.lock().unwrap();

        for tunnel in &*values {
            if tunnel.port_type != proto && tunnel.port_type != PortType::Both {
                continue;
            }

            if !tunnel.match_ip.matches(ip) {
                continue;
            }

            if tunnel.from_port <= port && port < tunnel.to_port {
                return Some(AddressValue {
                    value: tunnel.local_start_address,
                    from_port: tunnel.from_port,
                    to_port: tunnel.to_port,
                });
            }
        }

        None
    }
}

impl LocalLookup {
    pub async fn update(&self, tunnels: Vec<AgentTunnel>) {
        let mut entries: Vec<TunnelEntry> = vec![];

        for tunnel in tunnels {
            entries.push(TunnelEntry {
                pub_address: if tunnel.tunnel_type.as_ref().map(|v| v.eq("minecraft-java")).unwrap_or(false) {
                    tunnel.custom_domain.unwrap_or(tunnel.assigned_domain)
                } else {
                    format!("{}:{}", tunnel.custom_domain.unwrap_or(tunnel.assigned_domain), tunnel.port.from)
                },
                match_ip: MatchIp { ip_number: tunnel.ip_num, region_id: if tunnel.region_num == 0 { None } else { Some(tunnel.region_num) } },
                port_type: tunnel.proto,
                from_port: tunnel.port.from,
                to_port: tunnel.port.to,
                local_start_address: SocketAddr::new(tunnel.local_ip, tunnel.local_port),
            });
        }

        let mut value = self.data.lock().unwrap();
        *value = entries;
    }
}

pub struct TunnelEntry {
    pub pub_address: String,
    pub match_ip: MatchIp,
    pub port_type: PortType,
    pub from_port: u16,
    pub to_port: u16,
    pub local_start_address: SocketAddr,
}
