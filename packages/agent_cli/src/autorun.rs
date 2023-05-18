use std::{
    net::{IpAddr, SocketAddr},
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};

use playit_agent_core::{
    api::api::{AccountTunnelAllocation, AccountTunnels, PortType, ReqTunnelsList, TunnelOrigin},
    network::address_lookup::{AddressLookup, AddressValue},
    tunnel_runner::TunnelRunner,
    utils::now_milli,
};

use crate::{match_ip::MatchIp, playit_secret::PlayitSecret, ui::UI, CliError};

pub async fn autorun(ui: &mut UI, mut secret: PlayitSecret) -> Result<(), CliError> {
    let secret_code = secret
        .with_default_path()
        .ensure_valid(ui)
        .await?
        .get_or_setup(ui)
        .await?;

    let api = secret.create_api().await?;

    tokio::time::sleep(Duration::from_secs(2)).await;

    let lookup = {
        let account_tunnels = api
            .tunnels_list(ReqTunnelsList {
                tunnel_id: None,
                agent_id: None,
            })
            .await?;

        let lookup = Arc::new(LocalLookup {
            data: Mutex::new(vec![]),
        });
        lookup.update(account_tunnels).await;

        lookup
    };

    let mut error_count = 0;

    ui.write_screen("starting up tunnel connection")?;

    let runner = loop {
        match TunnelRunner::new(secret_code.clone(), lookup.clone()).await {
            Ok(res) => break res,
            Err(error) => {
                error_count += 1;
                if error_count > 5 {
                    ui.write_error("Final attempted failed to setup tunnel", &error)?;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    return Err(CliError::TunnelSetupError(error));
                };

                ui.write_error("Failed to setup tunnel client", error)?;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };

    let signal = runner.keep_running();
    let runner = tokio::spawn(runner.run());

    ui.write_screen("tunnel running")?;

    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;

        let account_tunnels_res = api
            .tunnels_list(ReqTunnelsList {
                tunnel_id: None,
                agent_id: None,
            })
            .await;

        let account_tunnels = match account_tunnels_res {
            Ok(v) => v,
            Err(error) => {
                ui.write_error("Failed to load latest tunnels", error)?;
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        ui.write_screen(format!(
            "{}: {} tunnel running, {} tunnels regisgered",
            env!("CARGO_PKG_VERSION"),
            now_milli(),
            account_tunnels.tunnels.len()
        ))?;

        lookup.update(account_tunnels).await;
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
    pub async fn update(&self, data: AccountTunnels) {
        let mut entries: Vec<TunnelEntry> = vec![];

        for tunnel in data.tunnels {
            match tunnel.alloc {
                AccountTunnelAllocation::Allocated(allocated) => {
                    let ip = match allocated.tunnel_ip {
                        IpAddr::V6(ip) => ip,
                        _ => continue,
                    };

                    let local_addr = match tunnel.origin {
                        TunnelOrigin::Default(def) => SocketAddr::new(
                            def.local_ip,
                            def.local_port.unwrap_or(allocated.port_start),
                        ),
                        TunnelOrigin::Agent(def) => SocketAddr::new(
                            def.local_ip,
                            def.local_port.unwrap_or(allocated.port_start),
                        ),
                        _ => continue,
                    };

                    let address = allocated.assigned_srv.unwrap_or(format!(
                        "{}:{}",
                        allocated.assigned_domain, allocated.port_start
                    ));

                    entries.push(TunnelEntry {
                        pub_address: address,
                        match_ip: MatchIp::new(ip),
                        port_type: tunnel.port_type,
                        from_port: allocated.port_start,
                        to_port: allocated.port_end,
                        local_start_address: local_addr,
                    });
                }
                _ => continue,
            }
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