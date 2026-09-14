use std::{future::Future, net::SocketAddr};

use playit_api_client::{
    PlayitApi,
    api::{AgentVersion, Platform, ReqAgentsRoutingGet, ReqProtoRegister},
};

use crate::{PROTOCOL_VERSION, error::SetupError};

pub const CONTROL_PORT: u16 = 5525;

/// The HTTP API calls the control channel depends on. Abstracted so the channel can be
/// exercised against a fake tunnel server.
pub trait ControlApi: Clone + Send + Sync + 'static {
    /// Control server addresses in preference order.
    fn control_addresses(&self)
    -> impl Future<Output = Result<Vec<SocketAddr>, SetupError>> + Send;

    /// Returns the encoded, signed `AgentRegister` request for the addresses the tunnel
    /// server reported seeing.
    fn sign_register(
        &self,
        client_addr: SocketAddr,
        tunnel_addr: SocketAddr,
    ) -> impl Future<Output = Result<Vec<u8>, SetupError>> + Send;
}

#[derive(Clone)]
pub struct PlayitControlApi {
    client: PlayitApi,
    version: AgentVersion,
    platform: Platform,
}

impl PlayitControlApi {
    pub fn new(client: PlayitApi, version: AgentVersion, platform: Platform) -> Self {
        PlayitControlApi {
            client,
            version,
            platform,
        }
    }

    pub fn client(&self) -> &PlayitApi {
        &self.client
    }
}

impl ControlApi for PlayitControlApi {
    async fn control_addresses(&self) -> Result<Vec<SocketAddr>, SetupError> {
        let routing = self
            .client
            .agents_routing_get(ReqAgentsRoutingGet { agent_id: None })
            .await?;

        let mut addresses = Vec::with_capacity(routing.targets6.len() + routing.targets4.len());
        if !routing.disable_ip6 {
            addresses.extend(
                routing
                    .targets6
                    .into_iter()
                    .map(|ip| SocketAddr::new(ip.into(), CONTROL_PORT)),
            );
        }
        addresses.extend(
            routing
                .targets4
                .into_iter()
                .map(|ip| SocketAddr::new(ip.into(), CONTROL_PORT)),
        );

        Ok(addresses)
    }

    async fn sign_register(
        &self,
        client_addr: SocketAddr,
        tunnel_addr: SocketAddr,
    ) -> Result<Vec<u8>, SetupError> {
        let signed = self
            .client
            .proto_register(ReqProtoRegister {
                agent_version: None,
                proto_version: PROTOCOL_VERSION,
                version: self.version.clone(),
                platform: self.platform,
                client_addr,
                tunnel_addr,
            })
            .await?;

        hex::decode(&signed.key).map_err(|_| SetupError::InvalidSignedKey)
    }
}
