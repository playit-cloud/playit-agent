use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use playit_agent_core::{
    agent_control::errors::SetupError,
    network::{
        origin_lookup::OriginLookup, tcp::tcp_settings::TcpSettings, udp::udp_settings::UdpSettings,
    },
    playit_agent::{PlayitAgent, PlayitAgentSettings},
};
use playit_api_client::{PlayitApi, api::ApiErrorNoFail, http_client::HttpClientError};
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;

use crate::{
    API_BASE,
    service::{
        client_server::{ServiceProto, ServiceRequest},
        messages::{PlayitServiceRequest, PlayitServiceResponse, PlayitServiceStatus},
    },
};

pub struct PlayitServiceProto;
impl ServiceProto for PlayitServiceProto {
    type Request = PlayitServiceRequest;
    type Response = PlayitServiceResponse;

    fn socket_id() -> &'static str {
        "playit"
    }
}

pub struct PlayitService {
    pub secret_key: String,
    pub rx: Receiver<ServiceRequest<PlayitServiceProto>>,
}

impl PlayitService {
    pub async fn start(self) -> PlayitServiceExitReason {
        let api_url = API_BASE.to_string();

        let lookup = Arc::new(OriginLookup::default());
        let client = PlayitApi::create(api_url.clone(), Some(self.secret_key.clone()));

        match client.agents_rundata().await {
            Ok(data) => lookup.update_from_run_data(&data).await,
            Err(error) => return PlayitServiceExitReason::FailedToLoadInitialRunData(error),
        };

        let agent_res = PlayitAgent::new(
            PlayitAgentSettings {
                api_url: API_BASE.to_string(),
                secret_key: self.secret_key.clone(),
                tcp_settings: TcpSettings::default(),
                udp_settings: UdpSettings::default(),
            },
            lookup.clone(),
        )
        .await;

        let run_data_errors = Arc::new(AtomicU64::new(0));
        let cancel = CancellationToken::new();

        let repeat_errors = run_data_errors.clone();
        tokio::spawn(cancel.clone().run_until_cancelled_owned(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;

                let data = match client.agents_rundata().await {
                    Ok(data) => data,
                    Err(error) => {
                        repeat_errors.fetch_add(1, Ordering::SeqCst);
                        tracing::error!(?error, "failed to load run data from API");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };

                repeat_errors.store(0, Ordering::SeqCst);
                lookup.update_from_run_data(&data).await;
            }
        }));

        let service_cancel = cancel.clone();
        let mut rx = self.rx;

        tokio::spawn(async move {
            while let Some(Some(req)) = service_cancel.run_until_cancelled(rx.recv()).await {
                let res = match req.msg {
                    PlayitServiceRequest::Stop => PlayitServiceResponse::ShuttingDown,
                    PlayitServiceRequest::Status => PlayitServiceResponse::Status({
                        if run_data_errors.load(Ordering::SeqCst) == 0 {
                            PlayitServiceStatus::Running
                        } else {
                            PlayitServiceStatus::FailingToLoadDataFromApi
                        }
                    }),
                };

                if !req.sender.send(res).await {
                    tracing::error!("failed to send response in service");
                    break;
                }
            }
            service_cancel.cancel();
        });

        match agent_res {
            Ok(agent) => {
                cancel.run_until_cancelled(agent.run()).await;
                cancel.cancel();
                PlayitServiceExitReason::AgentStopped
            }
            Err(error) => PlayitServiceExitReason::SetupError(error),
        }
    }
}

#[derive(Debug)]
pub enum PlayitServiceExitReason {
    SetupError(SetupError),
    FailedToLoadInitialRunData(ApiErrorNoFail<HttpClientError>),
    AgentStopped,
}
