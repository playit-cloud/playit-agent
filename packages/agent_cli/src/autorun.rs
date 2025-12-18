use std::{fmt::Write, net::SocketAddr, sync::Arc, time::Duration};

use playit_agent_core::{
    network::{
        origin_lookup::{OriginLookup, OriginResource, OriginTarget},
        tcp::tcp_settings::TcpSettings,
        udp::udp_settings::UdpSettings,
    },
    playit_agent::{PlayitAgent, PlayitAgentSettings},
    utils::now_milli,
};
use playit_agent_proto::PortProto;
use playit_api_client::api::*;
// use playit_ping_monitor::PingMonitor;

use crate::{API_BASE, CliError, playit_secret::PlayitSecret, ui::UI};

pub async fn autorun(ui: &mut UI, mut secret: PlayitSecret) -> Result<(), CliError> {
    let secret_code = secret.ensure_valid(ui).await?.get_or_setup(ui).await?;

    let api = secret.create_api().await?;
    // let mut ping_monitor = PingMonitor::new(api.clone()).await.unwrap();

    // /* start ping monitor */
    // tokio::spawn(async move {
    //     loop {
    //         if let Err(error) = ping_monitor.refresh().await {
    //             tracing::error!(?error, "error running ping monitor");
    //         }
    //         tokio::time::sleep(Duration::from_millis(3_000 + (random::<u64>() % 5_000))).await;
    //     }
    // });

    tokio::time::sleep(Duration::from_secs(2)).await;

    let lookup = Arc::new(OriginLookup::default());
    lookup
        .update_from_run_data(&api.agents_rundata().await?)
        .await;

    let mut error_count = 0;
    ui.write_screen("starting up tunnel connection").await;

    let settings = PlayitAgentSettings {
        udp_settings: UdpSettings::default(),
        tcp_settings: TcpSettings::default(),
        api_url: API_BASE.to_string(),
        secret_key: secret_code.clone(),
    };

    let runner = loop {
        match PlayitAgent::new(settings.clone(), lookup.clone()).await {
            Ok(res) => break res,
            Err(error) => {
                error_count += 1;
                if error_count > 5 {
                    ui.write_error("Final attempted failed to setup tunnel", &error)
                        .await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    return Err(CliError::TunnelSetupError(error));
                };

                ui.write_error("Failed to setup tunnel client", error).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };

    tokio::spawn(runner.run());

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

        lookup.update_from_run_data(&agent_data).await;

        let mut msg = format!(
            "playit (v{}): {} tunnel running, {} tunnels registered\n\n",
            env!("CARGO_PKG_VERSION"),
            now_milli(),
            agent_data.tunnels.len()
        );

        match agent_data.account_status {
            AgentAccountStatus::Guest => 'login_link: {
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

                        let link = format!(
                            "https://playit.gg/login/guest-account/{}",
                            session.session_key
                        );
                        writeln!(msg, "login: {}", link).unwrap();

                        guest_login_link = Some((link, now_milli()));
                    }
                }
            }
            AgentAccountStatus::EmailNotVerified => {
                writeln!(
                    msg,
                    "Email not verified https://playit.gg/account/settings/account/verify-email"
                )
                .unwrap();
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
                writeln!(
                    msg,
                    "Account disabled: https://playit.gg/account/agents/{}",
                    agent_data.agent_id
                )
                .unwrap();
            }
            AgentAccountStatus::Ready => {}
        }

        writeln!(msg, "\nTUNNELS").unwrap();

        if agent_data.tunnels.is_empty() && agent_data.pending.is_empty() {
            let agent_id = match agent_data.agent_type {
                AgentType::Default => "default".to_string(),
                AgentType::Assignable => agent_data.agent_id.to_string(),
                AgentType::SelfManaged => agent_data.agent_id.to_string(),
            };

            writeln!(
                msg,
                "Add tunnels here: https://playit.gg/account/agents/{}",
                agent_id
            )
            .unwrap();
        } else {
            for tunnel in &agent_data.tunnels {
                let addr = tunnel
                    .custom_domain
                    .as_ref()
                    .unwrap_or(&tunnel.assigned_domain);

                let src = match tunnel.tunnel_type.as_deref() {
                    Some("minecraft-java") => addr.clone(),
                    Some("https") => format!("https://{addr}"),
                    _ => format!("{}:{}", addr, tunnel.port.from),
                };

                if let Some(disabled) = tunnel.disabled {
                    let dst = format!("{}:{}", tunnel.local_ip, tunnel.local_port);
                    writeln!(msg, "{} => {} (disabled)", src, dst).unwrap();
                    if disabled == AgentTunnelDisabled::BySystem {
                        writeln!(
                            msg,
                            "\tsee: https://playit.gg/account/tunnels/{}",
                            tunnel.id
                        )
                        .unwrap();
                    }
                } else {
                    let res = OriginResource::from_agent_tunnel(&tunnel);

                    match res.target {
                        OriginTarget::Https {
                            ip,
                            http_port,
                            https_port,
                        } => {
                            writeln!(
                                msg,
                                "{} => {} (http: {}, https: {})",
                                src, ip, http_port, https_port
                            )
                            .unwrap();
                        }
                        OriginTarget::Port { ip, port } => {
                            if let Some(tunnel_type) = &tunnel.tunnel_type {
                                writeln!(msg, "{} => {}:{} ({})", src, ip, port, tunnel_type)
                                    .unwrap();
                            } else {
                                writeln!(
                                    msg,
                                    "{} => {}:{} (proto: {:?}, port count: {})",
                                    src,
                                    ip,
                                    port,
                                    tunnel.proto,
                                    tunnel.port.to - tunnel.port.from
                                )
                                .unwrap();
                            }
                        }
                    }
                }
            }

            for tunnel in &agent_data.pending {
                if tunnel.is_disabled {
                    writeln!(
                        msg,
                        "tunnel pending (disabled): https://playit.gg/account/tunnels/{}",
                        tunnel.id
                    )
                    .unwrap();
                } else {
                    writeln!(
                        msg,
                        "tunnel pending: https://playit.gg/account/tunnels/{}",
                        tunnel.id
                    )
                    .unwrap();
                }
            }
        }

        ui.write_screen(msg).await;
    }
}
