use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{Stdout, stdout};
use uuid::Uuid;
use playit_agent_core::api::api::{AccountTunnelAllocation, PlayitApiClient, PortType, TunnelType};
use playit_agent_core::api::http_client::HttpClient;
use playit_agent_core::api::PlayitApi;

use playit_agent_core::tunnel_runner::TunnelRunner;
use playit_agent_proto::PortProto;

use crate::{API_BASE, claim_exchange, claim_generate, claim_url, CliError, LookupWithOverrides, MappingOverride, tunnels_prepare};

#[derive(Serialize, Deserialize)]
pub struct LaunchConfig {
    pub agent_name: String,
    pub agent_type: Option<String>,

    pub secret_key: Option<String>,
    pub secret_path: Option<String>,

    pub command: String,
    pub command_args: Vec<String>,

    #[serde(default)]
    pub env_overrides: HashMap<String, String>,
    pub tunnels: Vec<Tunnel>,

    #[serde(default = "default_as_true")]
    pub special_lan: bool,
}

fn default_as_true() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
pub struct Tunnel {
    pub id: Option<Uuid>,
    pub tunnel_type: Option<TunnelType>,
    pub name: String,
    pub proto: PortType,
    pub port_count: u16,
    pub local: Option<u16>,
}

pub async fn launch(config: LaunchConfig) -> Result<(), anyhow::Error> {
    let secret = match config.get_secret().await? {
        Some(v) => v,
        None => config.setup_new(&config.secret_path.as_ref().unwrap()).await?,
    };

    let api = PlayitApi::create(
        API_BASE.to_string(),
        Some(secret.clone()),
    );

    let mut tunnels = Vec::new();
    let mut mapping_overrides = Vec::new();

    for tunnel_config in &config.tunnels {
        let tunnel = tunnels_prepare(
            &api,
            Some(tunnel_config.name.clone()),
            tunnel_config.tunnel_type.clone(),
            tunnel_config.proto,
            tunnel_config.port_count,
            false,
            false,
        ).await?;

        tunnels.push(tunnel.clone());

        let over = tunnel_config.local.and_then(|port|
            MappingOverride::new(tunnel, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
        );

        if let Some(over) = over {
            mapping_overrides.push(over);
        }
    }

    let mut command = tokio::process::Command::new(config.command);

    let mut i = 0;
    for tunnel in &tunnels {
        match &tunnel.alloc {
            AccountTunnelAllocation::Allocated(alloc) => {
                command.env(format!("TUNNEL_{}_PORT", i), alloc.port_start.to_string());
                command.env(format!("TUNNEL_{}_HOSTNAME", i), alloc.assigned_domain.clone());
                command.env(format!("TUNNEL_{}_HOSTNAME_SHORT", i), alloc.ip_hostname.clone());
                i += 1;
            }
            _ => continue,
        }
    }
    command.envs(config.env_overrides.iter());
    command.args(config.command_args);

    tracing::info!("setting up connection to tunnel server");

    let mut tunnel = TunnelRunner::new(
        secret,
        Arc::new(LookupWithOverrides(mapping_overrides)),
    ).await?;

    tunnel.set_use_special_lan(config.special_lan);

    let keep_running = tunnel.keep_running();

    tracing::info!("processing connection to tunnel server");
    let tunnel_task = tokio::spawn(tunnel.run());

    tracing::info!(?command, "starting command");
    let mut child = command.spawn()?;

    let exit_status = child.wait().await?;

    tracing::info!(?exit_status, "program closed");
    keep_running.store(false, Ordering::SeqCst);
    tunnel_task.await?;

    Ok(())
}

impl LaunchConfig {
    pub async fn get_secret(&self) -> Result<Option<String>, CliError> {
        if self.secret_key.is_some() {
            return Ok(self.secret_key.clone());
        }

        if let Some(path) = &self.secret_path {
            if let Ok(data) = tokio::fs::read_to_string(path).await {
                return Ok(Some(data.trim().to_string()));
            }
        }

        if let Some(res) = option_env!("PLAYIT_SECRET") {
            return Ok(Some(res.to_string()));
        }

        if self.secret_path.is_none() {
            tracing::error!("secret_path required when no secret is provided");
            return Err(CliError::MissingSecret);
        }

        Ok(None)
    }

    pub async fn setup_new(&self, secret_path: &str) -> Result<String, CliError> {
        let claim_code = claim_generate();

        let mut tcp_ports = 0;
        let mut udp_ports = 0;

        for tunnel in &self.tunnels {
            match tunnel.proto {
                PortType::Both => {
                    tcp_ports += tunnel.port_count;
                    udp_ports += tunnel.port_count;
                }
                PortType::Tcp => {
                    tcp_ports += tunnel.port_count;
                }
                PortType::Udp => {
                    udp_ports += tunnel.port_count;
                }
            }
        }

        let url = claim_url(&claim_code)?;

        let secret = loop {
            println!("Visit URL to setup:\n{}", url);

            if let Some(v) = claim_exchange(&claim_code, 4).await? {
                break v;
            }
        };

        if let Err(error) = tokio::fs::write(secret_path, &secret).await {
            tracing::error!(?error, secret_path, "failed to write secret file");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        Ok(secret)
    }
}