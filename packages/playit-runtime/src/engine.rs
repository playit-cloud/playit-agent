use std::sync::Arc;

use playit_agent_core::agent_control::errors::SetupError;
use playit_agent_core::gateway::{GatewayErrorCode, GatewayOrigin, PlayitGateway};
use playit_agent_core::network::origin_lookup::OriginLookup;
use playit_agent_core::playit_agent::{PlayitAgent, PlayitAgentSettings};
use playit_agent_core::stats::AgentStats;
use playit_model::TrafficSnapshot;

use crate::{EngineChild, EnginePort, EngineStartError, OriginPublisher, TrafficSource};

#[derive(Clone, Debug, Default)]
pub struct SupervisedEnginePort {
    settings: PlayitAgentSettings,
}

impl SupervisedEnginePort {
    pub fn new(settings: PlayitAgentSettings) -> Self {
        Self { settings }
    }
}

#[async_trait::async_trait]
impl EnginePort for SupervisedEnginePort {
    async fn start(
        &self,
        gateway: Arc<dyn PlayitGateway>,
        origins: &[GatewayOrigin],
    ) -> Result<EngineChild, EngineStartError> {
        let lookup = Arc::new(OriginLookup::default());
        lookup.update_from_gateway(origins).await;
        let agent = PlayitAgent::new(self.settings.clone(), lookup.clone(), gateway)
            .await
            .map_err(map_engine_start_error)?;
        let stats = agent.stats();
        let cancel = agent.cancellation_token();
        let exit = tokio::spawn(agent.run());
        Ok(EngineChild::new(
            cancel,
            exit,
            Arc::new(AgentOrigins(lookup)),
            Arc::new(AgentTraffic(stats)),
        ))
    }
}

struct AgentOrigins(Arc<OriginLookup>);

#[async_trait::async_trait]
impl OriginPublisher for AgentOrigins {
    async fn replace(&self, origins: &[GatewayOrigin]) {
        self.0.update_from_gateway(origins).await;
    }
}

struct AgentTraffic(AgentStats);

impl TrafficSource for AgentTraffic {
    fn snapshot(&self) -> TrafficSnapshot {
        let snapshot = self.0.snapshot();
        TrafficSnapshot {
            bytes_in: snapshot.bytes_in,
            bytes_out: snapshot.bytes_out,
            active_tcp: snapshot.active_tcp,
            active_udp: snapshot.active_udp,
        }
    }
}

fn map_engine_start_error(error: SetupError) -> EngineStartError {
    match &error {
        SetupError::Gateway(error) if error.code == GatewayErrorCode::InvalidSecret => {
            EngineStartError::InvalidSecret(
                "The configured playit secret is no longer valid. Run `playit setup` to provision a new secret."
                    .to_owned(),
            )
        }
        SetupError::Gateway(error)
            if error.code == GatewayErrorCode::AgentDisabledOverLimit =>
        {
            EngineStartError::AgentDisabledOverLimit(agent_limit_message())
        }
        _ => EngineStartError::Failed(setup_error_message(&error)),
    }
}

pub fn setup_error_message(error: &SetupError) -> String {
    match error {
        SetupError::FailedToConnect => {
            "Could not connect to playit tunnel servers. Check your internet connection, firewall, VPN, or DNS settings, then restart playit."
                .to_owned()
        }
        SetupError::Timeout(_) => {
            "Timed out while connecting to playit. Check your network/firewall and try again."
                .to_owned()
        }
        SetupError::IoError(error) => format!("Could not open a required network socket: {error}"),
        SetupError::Gateway(error) => match error.code {
            GatewayErrorCode::AgentDisabledOverLimit => agent_limit_message(),
            GatewayErrorCode::Transport => {
                "Could not reach the playit API. Check your internet connection or try again later."
                    .to_owned()
            }
            _ => format!("Failed to start the playit agent: {error}"),
        },
        _ => format!("Failed to start the playit agent: {error}"),
    }
}

fn agent_limit_message() -> String {
    "This account is over the agent limit. Delete an unused agent or upgrade the account, then the service will retry."
        .to_owned()
}
