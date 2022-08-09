use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use tokio::sync::RwLock;
use tracing_subscriber::fmt::time;

use playit_agent_common::{ClaimError, ClaimLease, NewClient, RpcMessage, SetupUdpChannelDetails, TunnelFeed, TunnelResponse};
use playit_agent_common::agent_config::{AgentConfig, AgentConfigBuilder};
use playit_agent_common::auth::{MAX_SESSION_AGE, SignatureError};

use crate::now_milli;
use crate::udp_client::UdpTunnel;

use super::agent_state::AgentState;
use super::api_client::ApiClient;
use super::tunnel_api::{TunnelApi, TunnelApiError};

pub struct AgentUpdater {
    api: TunnelApi,
    state: Arc<AgentState>,
}

impl AgentUpdater {
    pub fn new(api: TunnelApi, state: AgentState) -> Self {
        AgentUpdater {
            api,
            state: Arc::new(state),
        }
    }

    pub fn state(&self) -> Arc<AgentState> {
        self.state.clone()
    }

    pub fn tunnel_api(&self) -> &TunnelApi {
        &self.api
    }

    pub fn web_api(&self) -> &ApiClient {
        self.api.client_api()
    }

    pub async fn process_tunnel_feed(self: &Arc<Self>) -> Result<Option<NewClient>, TunnelApiError> {
        let msg = match self.api.io().await.recv().await {
            Ok(v) => v,
            Err(error) => {
                tracing::error!(?error, "failed to read next feed message");
                return Err(TunnelApiError::IoError(error));
            }
        };

        self.state.last_rcp_received_at.store(now_milli(), Ordering::SeqCst);

        match msg {
            TunnelFeed::Response(resp) => {
                self.handle(resp).await?;
                Ok(None)
            }
            TunnelFeed::NewClientV4(new_client) => Ok(Some(new_client.into())),
            TunnelFeed::NewClientV6(new_client) => Ok(Some(new_client.into())),
        }
    }

    pub async fn send_ping(&self) -> Result<(), TunnelApiError> {
        let ping = self.tunnel_api().ping(now_milli());
        self.tunnel_api().io().await.send(ping).await?;
        Ok(())
    }

    async fn handle(self: &Arc<Self>, response: RpcMessage<TunnelResponse>) -> Result<(), TunnelApiError> {
        let start = now_milli();

        let msg = format!("{:?}", response.content);

        let done = match response.content {
            TunnelResponse::AgentRegistered(reg) => {
                self.state.authenticate_times.ack();

                tracing::info!(?reg, "agent registered");

                let this = self.clone();
                tokio::spawn(async move {
                    if let Err(error) = this.api.register(reg).await {
                        tracing::error!(?error, "failed to register");
                        this.state.authenticate_times.remove_ack();
                    }
                });

                Ok(())
            }
            TunnelResponse::ClaimResponseV2(Ok(lease)) => {
                let mut port_claims = self.state.port_claims.write().await;
                port_claims.ack_request(now_milli(), response.request_id, self.tunnel_api().io().await.request_id());
                self.state.port_claim_times.set_ack(port_claims.min_ack());

                tracing::info!(?lease, "lease claimed");
                Ok(())
            }
            TunnelResponse::ClaimResponseV2(Err(error)) => {
                tracing::info!(?error, "failed to claim lease");

                match error {
                    ClaimError::NotRegistered => {
                        self.state.authenticate_times.remove_ack();
                    }
                    _ => {}
                }

                Ok(())
            }
            TunnelResponse::KeptAlive(keep_alive) => {
                if !keep_alive.alive {
                    tracing::info!("register needed");
                    self.state.authenticate_times.remove_ack();
                } else {
                    tracing::info!("keep alive");
                    self.state.keep_alive_times.ack();
                }

                self.state.connected_server_id.store(keep_alive.tunnel_server_id, Ordering::SeqCst);
                Ok(())
            }
            TunnelResponse::Pong(pong) => {
                let now_ms = now_milli();
                self.state.latency.store(now_ms - pong.id, Ordering::SeqCst);
                self.state.latency_update.store(now_ms, Ordering::SeqCst);
                self.state.connected_server_id.store(pong.tunnel_server_id, Ordering::SeqCst);

                Ok(())
            }
            TunnelResponse::SignatureError(error) => {
                match error {
                    SignatureError::SignatureExpired { now, timestamp, from_system: false } if now.abs_diff(timestamp) > 3_000 => {
                        let time_adjust = now as i64 - timestamp as i64;
                        tracing::warn!(time_adjust, "signature expired due to bad clock, updating clock");
                        self.tunnel_api().set_time_adjust(time_adjust);
                        self.state.authenticate_times.remove_ack();
                    }
                    SignatureError::SignatureExpired { .. } => {
                        tracing::info!("signature expired");
                        self.state.authenticate_times.remove_ack();
                    }
                    error => {
                        tracing::error!(?error, "got signature error");
                    }
                }

                Ok(())
            }
            TunnelResponse::SetupUdpChannelDetails(setup) => {
                self.state.udp_channel_times.ack();
                self.save_udp_channel_details(setup.into()).await;
                Ok(())
            }
            TunnelResponse::SetupUdpChannelDetailsV6(setup) => {
                self.state.udp_channel_times.ack();
                self.save_udp_channel_details(setup.into()).await;
                Ok(())
            }
            TunnelResponse::BadRequest => {
                tracing::error!(request_id = response.request_id, "bad request sent");
                Ok(())
            }
            TunnelResponse::Failed => {
                tracing::error!(request_id = response.request_id, "request failed");
                Ok(())
            }
            TunnelResponse::ClaimResponse(claim) => {
                tracing::error!(?claim, "got old ClaimResponse format");
                Ok(())
            }
        };

        let elapsed = now_milli() - start;
        if elapsed > 10 {
            tracing::info!(?msg, elapsed, "slow feed message");
        }

        done
    }

    async fn save_udp_channel_details(&self, details: SetupUdpChannelDetails) {
        tracing::info!(?details, "register udp channel details");

        let tunnel_addr = details.tunnel_addr;

        {
            let mut lock = self.state.udp_channel_details.write().await;
            *lock = Some(details);
        }

        {
            let mut lock = self.state.udp_tunnel.write().await;
            if lock.is_none() {
                let tunnel = match UdpTunnel::new(self.state.clone(), tunnel_addr).await {
                    Ok(v) => Arc::new(v),
                    Err(error) => {
                        tracing::error!(?error, "failed to setup UDP tunnel");
                        self.state.udp_channel_times.remove_ack();
                        return;
                    }
                };

                tokio::spawn(tunnel.clone().run_keepalive());
                tokio::spawn(tunnel.clone().handle_packets());

                *lock = Some(tunnel);
            }
        }
    }

    pub async fn update(&self) -> Result<u64, TunnelApiError> {
        let mut wait = 30_000;

        wait = wait.min(self.update_config().await?);
        wait = wait.min(self.register().await?);
        wait = wait.min(self.send_claims().await?);
        wait = wait.min(self.setup_udp_channel().await?);

        Ok(wait)
    }

    async fn update_config(&self) -> Result<u64, TunnelApiError> {
        let now = now_milli();

        /* see if we should update config */
        {
            let config = self.state.agent_config.read().await;

            match config.api_refresh_rate {
                None => return Ok(30_000),
                Some(wait) => {
                    let last_update = self.state.agent_config_update_at.load(Ordering::SeqCst);
                    if now < last_update + wait {
                        return Ok(last_update + wait - now);
                    }
                }
            }
        }

        let updated_config = self.api.client_api().get_agent_config().await?.build();
        let wait = match updated_config.api_refresh_rate {
            Some(v) => v,
            None => 30_000,
        };

        let mut agent_config = self.state.agent_config.write().await;
        if let Some(updated_config) = agent_config.get_updated(updated_config) {
            tracing::info!("agent config updated");

            if let Some(save_path) = &self.state.agent_config_save_path {
                match toml::to_string(&updated_config.as_builder()) {
                    Ok(config_string) => {
                        if let Err(error) = tokio::fs::write(save_path, config_string).await {
                            tracing::error!(?error, %save_path, "failed to save agent config to path");
                        }
                    }
                    Err(error) => {
                        tracing::error!(?error, "failed to generate toml version of agent config");
                    }
                }
            }
            *agent_config = Arc::new(updated_config);
        }

        Ok(wait)
    }

    async fn setup_udp_channel(&self) -> Result<u64, TunnelApiError> {
        if let Err(wait) = self.state.udp_channel_times.try_send(1_000, 15_000) {
            return Ok(wait);
        }

        let _ = self.api.setup_udp_channel().await?;
        self.sent_message();

        Ok(150_000)
    }

    async fn send_claims(&self) -> Result<u64, TunnelApiError> {
        if let Err(wait) = self.state.port_claim_times.try_send(3_000, 60_000) {
            return Ok(wait);
        }

        {
            let config = self.state.agent_config.read().await;
            let mut port_claims = self.state.port_claims.write().await;
            port_claims.set_target(&config, &self.tunnel_api().io().await.request_id());
        }

        let requests = {
            let port_claims = self.state.port_claims.read().await;
            port_claims.build_requests(now_milli())
        };

        for action in requests {
            if !action.add {
                /* TODO: add support for removing lease */
                continue;
            }

            self.api.claim_lease(action.lease, Some(action.request_id)).await?;
            self.sent_message();
        }

        Ok(15_000)
    }

    async fn register(&self) -> Result<u64, TunnelApiError> {
        let mut wait = 10_000;

        if let Err(wait_ms) = self.state.authenticate_times.try_send(1_000, MAX_SESSION_AGE / 2) {
            wait = wait_ms.min(wait);
        } else {
            let _ = self.api.request_register().await?;
            self.sent_message();
            return Ok(10_000);
        }

        if let Err(wait_ms) = self.state.keep_alive_times.try_send(1_000, 10_000) {
            wait = wait_ms.min(wait);
            return Ok(wait);
        }

        match self.api.keep_alive().await {
            Err(TunnelApiError::SessionNotRegistered) => {
                self.state.authenticate_times.remove_ack();
            }
            Err(error) => return Err(error),
            Ok(_) => {
                self.sent_message();
            }
        }

        Ok(10_000)
    }

    fn sent_message(&self) {
        self.state.last_rcp_sent_at.store(now_milli(), Ordering::SeqCst);
    }
}