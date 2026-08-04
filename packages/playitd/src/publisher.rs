use std::sync::Arc;
use std::time::Duration;

use playit_agent_core::network::origin_lookup::{OriginLookup, OriginResource, OriginTarget};
use playit_agent_core::stats::AgentStats;
use playit_agent_core::utils::now_milli;
use playit_api_client::PlayitApi;
use playit_api_client::api::AccountStatus;
use playit_ipc::ipc::protocol_info;
use playit_ipc::model::{
    AccountStatus as ServiceAccountStatus, AgentLifecycle, AgentState, ConnectionStats,
    NoticeState, PendingTunnelState, ServiceError, ServicePhase, ServiceStatus, ServiceUpdate,
    TunnelState,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::guest_login::GuestLoginCache;
use crate::ipc_server::StateCache;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunningSummary {
    pub(crate) tunnel_count: usize,
    pub(crate) pending_tunnel_count: usize,
    pub(crate) disabled_tunnel_count: usize,
    pub(crate) account_status: &'static str,
}

impl RunningSummary {
    pub(crate) fn from_state(state: &AgentState) -> Self {
        Self {
            tunnel_count: state.tunnels.len(),
            pending_tunnel_count: state.pending_tunnels.len(),
            disabled_tunnel_count: state
                .tunnels
                .iter()
                .filter(|tunnel| tunnel.is_disabled)
                .count(),
            account_status: service_account_status_label(&state.account_status),
        }
    }
}

pub(crate) struct StatusContext {
    pub(crate) secret_path: Option<String>,
    pub(crate) socket_path: String,
    pub(crate) version: String,
    pub(crate) start_time: u64,
}

impl StatusContext {
    pub(crate) fn status(
        &self,
        phase: ServicePhase,
        has_secret: bool,
        last_error: Option<ServiceError>,
    ) -> ServiceStatus {
        ServiceStatus {
            phase,
            pid: std::process::id(),
            uptime_secs: now_milli().saturating_sub(self.start_time) / 1000,
            version: self.version.clone(),
            socket_path: self.socket_path.clone(),
            secret_path: self.secret_path.clone(),
            has_secret,
            protocol: protocol_info(),
            last_error,
        }
    }
}

pub(crate) async fn publish_runtime_state(
    state_cache: &Arc<StateCache>,
    event_tx: &broadcast::Sender<ServiceUpdate>,
    status: ServiceStatus,
    lifecycle: AgentLifecycle,
) {
    state_cache.set_status(status.clone()).await;
    state_cache.set_lifecycle(lifecycle.clone()).await;
    let _ = event_tx.send(ServiceUpdate::Status(status));
    let _ = event_tx.send(ServiceUpdate::Lifecycle(lifecycle));
}

pub(crate) async fn broadcast_stats(
    stats: AgentStats,
    event_tx: broadcast::Sender<ServiceUpdate>,
    state_cache: Arc<StateCache>,
    cancel_token: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let snapshot = stats.snapshot();
                let stats = ConnectionStats {
                    bytes_in: snapshot.bytes_in,
                    bytes_out: snapshot.bytes_out,
                    active_tcp: snapshot.active_tcp,
                    active_udp: snapshot.active_udp,
                };
                state_cache.set_stats(stats.clone()).await;
                let _ = event_tx.send(ServiceUpdate::Stats(stats));
            }
            _ = cancel_token.cancelled() => break,
        }
    }
}

pub(crate) struct AgentStatePublisher {
    pub(crate) api: PlayitApi,
    pub(crate) lookup: Arc<OriginLookup>,
    pub(crate) event_tx: broadcast::Sender<ServiceUpdate>,
    pub(crate) state_cache: Arc<StateCache>,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) start_time: u64,
    pub(crate) version_string: String,
    pub(crate) guest_login_cache: Arc<GuestLoginCache>,
}

impl AgentStatePublisher {
    pub(crate) async fn run(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        let mut last_running_summary: Option<RunningSummary> = None;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.api.v1_agents_rundata().await {
                        Ok(mut api_data) => {
                            self.lookup.update_from_run_data(&api_data).await;

                            let login_link = match api_data.permissions.account_status {
                                AccountStatus::Guest => {
                                    self.guest_login_cache.get_or_create(&self.api).await.ok()
                                }
                                _ => None,
                            };

                            api_data.notices.sort_by_key(|notice| notice.priority);

                            let state = AgentState {
                                version: self.version_string.clone(),
                                tunnels: api_data
                                    .tunnels
                                    .iter()
                                    .filter_map(|tunnel| {
                                        let origin = OriginResource::from_agent_tunnel(tunnel)?;
                                        let destination = match origin.target {
                                            OriginTarget::Https {
                                                ip,
                                                http_port,
                                                https_port,
                                            } => format!(
                                                "{ip} (http: {http_port}, https: {https_port})"
                                            ),
                                            OriginTarget::Port { ip, port } => format!("{ip}:{port}"),
                                        };

                                        Some(TunnelState {
                                            display_address: tunnel.display_address.clone(),
                                            destination,
                                            is_disabled: tunnel.disabled_reason.is_some(),
                                            disabled_reason: tunnel
                                                .disabled_reason
                                                .as_ref()
                                                .map(ToString::to_string),
                                        })
                                    })
                                    .collect(),
                                pending_tunnels: api_data
                                    .pending
                                    .iter()
                                    .map(|pending| PendingTunnelState {
                                        id: pending.id.to_string(),
                                        status_msg: pending.status_msg.clone(),
                                    })
                                    .collect(),
                                notices: api_data
                                    .notices
                                    .iter()
                                    .map(|notice| NoticeState {
                                        priority: format!("{:?}", notice.priority),
                                        message: notice.message.to_string(),
                                        resolve_link: notice
                                            .resolve_link
                                            .as_ref()
                                            .map(ToString::to_string),
                                    })
                                    .collect(),
                                account_status: match api_data.permissions.account_status {
                                    AccountStatus::Guest => ServiceAccountStatus::Guest,
                                    AccountStatus::EmailNotVerified => {
                                        ServiceAccountStatus::EmailNotVerified
                                    }
                                    AccountStatus::Verified => ServiceAccountStatus::Verified,
                                },
                                agent_id: api_data.agent_id.to_string(),
                                login_link,
                                start_time: self.start_time,
                            };

                            let summary = RunningSummary::from_state(&state);
                            if last_running_summary.as_ref() != Some(&summary) {
                                if last_running_summary.is_none() {
                                    tracing::info!(
                                        agent_id = %state.agent_id,
                                        tunnel_count = summary.tunnel_count,
                                        pending_tunnel_count = summary.pending_tunnel_count,
                                        disabled_tunnel_count = summary.disabled_tunnel_count,
                                        account_status = summary.account_status,
                                        "playit connected; tunnels loaded"
                                    );
                                } else {
                                    tracing::info!(
                                        agent_id = %state.agent_id,
                                        tunnel_count = summary.tunnel_count,
                                        pending_tunnel_count = summary.pending_tunnel_count,
                                        disabled_tunnel_count = summary.disabled_tunnel_count,
                                        account_status = summary.account_status,
                                        "playit tunnel state updated"
                                    );
                                }
                                last_running_summary = Some(summary);
                            }

                            let lifecycle = AgentLifecycle::Running(state);
                            self.state_cache.set_lifecycle(lifecycle.clone()).await;
                            let _ = self.event_tx.send(ServiceUpdate::Lifecycle(lifecycle));
                        }
                        Err(error) => tracing::error!(?error, "Failed to load agent data"),
                    }
                }
                _ = self.cancel_token.cancelled() => break,
            }
        }
    }
}

fn service_account_status_label(status: &ServiceAccountStatus) -> &'static str {
    match status {
        ServiceAccountStatus::Unknown => "unknown",
        ServiceAccountStatus::Guest => "guest",
        ServiceAccountStatus::EmailNotVerified => "email_not_verified",
        ServiceAccountStatus::Verified => "verified",
    }
}
