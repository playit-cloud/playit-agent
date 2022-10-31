use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use playit_agent_proto::PortProto;

use crate::{claim_exchange, claim_generate, claim_url, CliError};

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
}

#[derive(Serialize, Deserialize)]
pub struct Tunnel {
    pub id: Option<Uuid>,
    pub name: String,
    pub proto: PortProto,
    pub port_count: u16,
    pub local: Option<u32>,
}

pub async fn launch(config: LaunchConfig) -> Result<(), CliError> {
    let secret = match config.get_secret().await {
        Some(v) => v,
        None => config.setup_new().await?,
    };
}

impl LaunchConfig {
    pub async fn get_secret(&self) -> Option<String> {
        if self.secret_key.is_some() {
            return self.secret_key.clone();
        }

        if let Some(path) = &self.secret_path {
            if let Ok(data) = tokio::fs::read_to_string(path).await {
                return Some(data);
            }
        }

        option_env!("PLAYIT_SECRET").map(|v| v.to_string())
    }

    pub async fn setup_new(&self) -> Result<String, CliError> {
        let claim_code = claim_generate();

        let mut tcp_ports = 0;
        let mut udp_ports = 0;

        for tunnel in &self.tunnels {
            match tunnel.proto {
                PortProto::Both => {
                    tcp_ports += tunnel.port_count;
                    udp_ports += tunnel.port_count;
                }
                PortProto::Tcp => {
                    tcp_ports += tunnel.port_count;
                }
                PortProto::Udp => {
                    udp_ports += tunnel.port_count;
                }
            }
        }

        let url = claim_url(&claim_code, &self.agent_name, "self-managed")?;

        loop {
            println!("Visit URL to setup:\n{}", url);

            if let Some(v) = claim_exchange(&claim_code, 4).await? {
                break Ok(v);
            }
        }
    }
}