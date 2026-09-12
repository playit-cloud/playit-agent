use std::{future::Future, net::SocketAddr};

pub use crate::network::packet_io::{DualStackUdpSocket, PacketIO, PacketRx, PacketTx};
use errors::SetupError;
use playit_agent_proto::control_messages::Pong;
use version::get_version;

pub use playit_api_client::api::SignedAgentKey;
use playit_api_client::{
    PlayitApi,
    api::{ReqAgentsRoutingGet, ReqProtoRegister},
};

use crate::{agent_control::platform::current_platform, utils::error_helper::ErrorHelper};

pub mod errors;

pub mod address_selector;
pub mod connected_control;
pub mod established_control;
pub mod maintained_control;
pub mod platform;
pub mod version;

pub trait AuthResource: Clone + Send + Sync + 'static {
    fn authenticate(
        &self,
        pong: &Pong,
    ) -> impl Future<Output = Result<SignedAgentKey, SetupError>> + Send;

    fn get_control_addresses(
        &self,
    ) -> impl Future<Output = Result<Vec<SocketAddr>, SetupError>> + Send;
}

#[derive(Clone)]
pub struct AuthApi {
    client: PlayitApi,
}

impl AuthApi {
    pub fn new(api_url: String, secret_key: String) -> Self {
        let client = PlayitApi::create(api_url, Some(secret_key));
        AuthApi { client }
    }
}

impl AuthResource for AuthApi {
    async fn authenticate(&self, pong: &Pong) -> Result<SignedAgentKey, SetupError> {
        let res = self
            .client
            .proto_register(ReqProtoRegister {
                agent_version: None,
                client_addr: pong.client_addr,
                tunnel_addr: pong.tunnel_addr,
                proto_version: 2,
                version: get_version(),
                platform: current_platform(),
            })
            .await
            .with_error(|error| tracing::error!(?error, "failed to sign and register"))?;

        Ok(res)
    }

    async fn get_control_addresses(&self) -> Result<Vec<SocketAddr>, SetupError> {
        let routing = self
            .client
            .agents_routing_get(ReqAgentsRoutingGet { agent_id: None })
            .await?;

        let mut addresses = vec![];
        for ip6 in routing.targets6 {
            addresses.push(SocketAddr::new(ip6.into(), 5525));
        }
        for ip4 in routing.targets4 {
            addresses.push(SocketAddr::new(ip4.into(), 5525));
        }

        Ok(addresses)
    }
}

#[cfg(test)]
mod tests;

fn next_request_id() -> u64 {
    use std::sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    };
    static NEXT: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(crate::utils::now_milli()));
    NEXT.fetch_add(1, Ordering::Relaxed)
}
