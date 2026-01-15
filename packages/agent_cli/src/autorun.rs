use std::{fmt::Write, net::SocketAddr, sync::Arc, time::Duration};

use playit_agent_core::{
    network::{
        origin_lookup::{OriginLookup, OriginResource, OriginTarget},
        tcp::tcp_settings::TcpSettings,
        udp::udp_settings::UdpSettings,
    },
    playit_agent::{PlayitAgent, PlayitAgentSettings},
    stats::AgentStats,
    utils::now_milli,
};
use playit_api_client::api::*;
use tokio::sync::mpsc;

use crate::{
    API_BASE, CliError,
    playit_secret::PlayitSecret,
    ui::{
        tui_app::{AccountStatusInfo, AgentData, ConnectionStats, NoticeInfo, PendingTunnelInfo, TunnelInfo},
        UI,
    },
};

pub async fn autorun(ui: &mut UI, mut secret: PlayitSecret) -> Result<(), CliError> {
    let secret_code = secret.ensure_valid(ui).await?.get_or_setup(ui).await?;

    let api = secret.create_api().await?;

    tokio::time::sleep(Duration::from_secs(2)).await;

    let lookup = Arc::new(OriginLookup::default());
    lookup
        .update_from_run_data(&api.v1_agents_rundata().await?)
        .await;

    let mut error_count = 0;
    ui.write_screen("starting up tunnel connection").await;

    let settings = PlayitAgentSettings {
        udp_settings: UdpSettings::default(),
        tcp_settings: TcpSettings::default(),
        api_url: API_BASE.to_string(),
        secret_key: secret_code.clone(),
    };

    let (runner, stats) = loop {
        match PlayitAgent::new(settings.clone(), lookup.clone()).await {
            Ok(res) => {
                let stats = res.stats();
                break (res, stats);
            }
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

    tracing::info!("tunnel running");

    // Run the appropriate UI loop
    if ui.is_tui() {
        run_tui_loop(ui, api, lookup, stats).await
    } else {
        run_log_only_loop(ui, api, lookup).await
    }
}

/// Run the TUI event loop with data updates
async fn run_tui_loop(
    ui: &mut UI,
    api: playit_api_client::PlayitApi,
    lookup: Arc<OriginLookup>,
    stats: AgentStats,
) -> Result<(), CliError> {
    let start_time = now_milli();
    let (data_tx, mut data_rx) = mpsc::channel::<AgentData>(4);

    // Spawn the data fetcher task
    let api_clone = api.clone();
    let lookup_clone = lookup.clone();
    tokio::spawn(async move {
        let mut guest_login_link: Option<(String, u64)> = None;
        let mut first_run = true;

        loop {
            // Don't delay on first run
            if first_run {
                first_run = false;
            } else {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }

            let account_tunnels_res = api_clone.v1_agents_rundata().await;
            let mut api_data = match account_tunnels_res {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "Failed to load latest tunnels");
                    continue;
                }
            };

            lookup_clone.update_from_run_data(&api_data).await;

            // Convert API data to TUI AgentData
            let account_status = match api_data.permissions.account_status {
                AccountStatus::Guest => AccountStatusInfo::Guest,
                AccountStatus::EmailNotVerified => AccountStatusInfo::EmailNotVerified,
                AccountStatus::Verified => AccountStatusInfo::Verified,
            };

            // Get login link for guest accounts
            let login_link = match api_data.permissions.account_status {
                AccountStatus::Guest => {
                    let now = now_milli();
                    match &guest_login_link {
                        Some((link, ts)) if now - *ts < 15_000 => Some(link.clone()),
                        _ => {
                            if let Ok(session) = api_clone.login_guest().await {
                                let link = format!(
                                    "https://playit.gg/login/guest-account/{}",
                                    session.session_key
                                );
                                guest_login_link = Some((link.clone(), now_milli()));
                                Some(link)
                            } else {
                                None
                            }
                        }
                    }
                }
                _ => None,
            };

            api_data.notices.sort_by_key(|n| n.priority);

            let notices: Vec<NoticeInfo> = api_data
                .notices
                .iter()
                .map(|n| NoticeInfo {
                    priority: format!("{:?}", n.priority),
                    message: n.message.to_string(),
                    resolve_link: n.resolve_link.as_ref().map(|s| s.to_string()),
                })
                .collect();

            let tunnels: Vec<TunnelInfo> = api_data
                .tunnels
                .iter()
                .filter_map(|tunnel| {
                    let origin = OriginResource::from_agent_tunnel(tunnel)?;

                    let destination = match origin.target {
                        OriginTarget::Https {
                            ip,
                            http_port,
                            https_port,
                        } => format!("{ip} (http: {http_port}, https: {https_port})"),
                        OriginTarget::Port { ip, port } => SocketAddr::new(ip, port).to_string(),
                    };

                    Some(TunnelInfo {
                        display_address: tunnel.display_address.clone(),
                        destination,
                        is_disabled: tunnel.disabled_reason.is_some(),
                        disabled_reason: tunnel.disabled_reason.as_ref().map(|s| s.to_string()),
                    })
                })
                .collect();

            let pending_tunnels: Vec<PendingTunnelInfo> = api_data
                .pending
                .iter()
                .map(|p| PendingTunnelInfo {
                    id: p.id.to_string(),
                    status_msg: p.status_msg.clone(),
                })
                .collect();

            let agent_data = AgentData {
                version: env!("CARGO_PKG_VERSION").to_string(),
                tunnels,
                pending_tunnels,
                notices,
                account_status,
                agent_id: api_data.agent_id.to_string(),
                login_link,
                start_time,
            };

            if data_tx.send(agent_data).await.is_err() {
                // UI has closed
                break;
            }
        }
    });

    // Run the TUI with data updates
    loop {
        // Check for new data
        while let Ok(data) = data_rx.try_recv() {
            ui.update_agent_data(data);
        }

        // Update stats from the agent
        let snapshot = stats.snapshot();
        ui.update_stats(ConnectionStats {
            bytes_in: snapshot.bytes_in,
            bytes_out: snapshot.bytes_out,
            active_tcp: snapshot.active_tcp,
            active_udp: snapshot.active_udp,
        });

        // Run one iteration of the TUI
        match ui.tick_tui() {
            Ok(true) => {}  // Continue
            Ok(false) => {
                // Quit requested
                ui.shutdown_tui()?;
                std::process::exit(0);
            }
            Err(e) => {
                ui.shutdown_tui()?;
                return Err(e);
            }
        }

        // Yield to allow other tasks to run
        tokio::task::yield_now().await;
    }
}

/// Run the log-only loop (original behavior)
async fn run_log_only_loop(
    ui: &mut UI,
    api: playit_api_client::PlayitApi,
    lookup: Arc<OriginLookup>,
) -> Result<(), CliError> {
    let mut guest_login_link: Option<(String, u64)> = None;

    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;

        let account_tunnels_res = api.v1_agents_rundata().await;
        let mut agent_data = match account_tunnels_res {
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

        match agent_data.permissions.account_status {
            AccountStatus::Guest => 'login_link: {
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
            AccountStatus::EmailNotVerified => {
                writeln!(
                    msg,
                    "Email not verified https://playit.gg/account/settings/account/verify-email"
                )
                .unwrap();
            }
            AccountStatus::Verified => {}
        }

        agent_data.notices.sort_by_key(|n| n.priority);

        for notice in &agent_data.notices {
            writeln!(msg, "[{:?}] {}", notice.priority, notice.message).unwrap();
            if let Some(link) = &notice.resolve_link {
                writeln!(msg, "{link}").unwrap();
            }
        }

        writeln!(msg, "\nTUNNELS").unwrap();

        if agent_data.tunnels.is_empty() && agent_data.pending.is_empty() {
            writeln!(
                msg,
                "Add tunnels here: https://playit.gg/account/agents/{}",
                agent_data.agent_id
            )
            .unwrap();
        } else {
            for tunnel in &agent_data.tunnels {
                let Some(origin) = OriginResource::from_agent_tunnel(&tunnel) else {
                    continue;
                };

                if let Some(reason) = &tunnel.disabled_reason {
                    writeln!(msg, "{} => (disabled {reason})", tunnel.display_address).unwrap();
                    continue;
                }

                let dst = match origin.target {
                    OriginTarget::Https {
                        ip,
                        http_port,
                        https_port,
                    } => format!("{ip} (http: {http_port}, https: {https_port})"),
                    OriginTarget::Port { ip, port } => SocketAddr::new(ip, port).to_string(),
                };

                writeln!(msg, "{} => {}", tunnel.display_address, dst).unwrap();
            }

            for tunnel in &agent_data.pending {
                writeln!(
                    msg,
                    "tunnel ({}): https://playit.gg/account/tunnels/{}",
                    tunnel.status_msg, tunnel.id
                )
                .unwrap();
            }
        }

        ui.write_screen(msg).await;
    }
}
