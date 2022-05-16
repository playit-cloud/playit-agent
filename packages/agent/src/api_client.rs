use std::net::{SocketAddr, SocketAddrV6};

use hyper::{Body, header, Method, Request};
use hyper::body::Buf;
use hyper::client::HttpConnector;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use serde::{Deserialize, Serialize};
use tracing::instrument::WithSubscriber;

use agent_common::{AgentRegistered, TunnelRequest};
use agent_common::agent_config::AgentConfig;
use agent_common::api::{AgentAccountStatus, AgentApiRequest, AgentApiResponse, ExchangeClaimForSecret, SessionSecret};
use agent_common::rpc::SignedRpcRequest;

pub struct ApiClient {
    api_base: String,
    agent_secret: Option<String>,
    client: hyper::Client<HttpsConnector<HttpConnector>, Body>,
}

impl ApiClient {
    pub fn new(api_base: String, agent_secret: Option<String>) -> Self {
        let connector = if api_base.starts_with("http://") {
            HttpsConnectorBuilder::new()
                .with_webpki_roots()
                .https_or_http()
                .enable_http1()
                .enable_http2()
                .build()
        } else {
            HttpsConnectorBuilder::new()
                .with_webpki_roots()
                .https_only()
                .enable_http1()
                .enable_http2()
                .build()
        };

        ApiClient {
            api_base,
            agent_secret,
            client: hyper::Client::builder().build(connector),
        }
    }

    pub async fn get_control_addr(&self) -> Result<SocketAddr, ApiError> {
        match self.req(&AgentApiRequest::GetControlAddr).await? {
            AgentApiResponse::ControlAddress(addr) => Ok(addr.control_address),
            resp => Err(ApiError::UnexpectedResponse(resp)),
        }
    }

    pub async fn get_agent_account_status(&self) -> Result<AgentAccountStatus, ApiError> {
        match self.req(&AgentApiRequest::GetAgentAccountStatus).await? {
            AgentApiResponse::AgentAccountStatus(status) => Ok(status),
            resp => Err(ApiError::UnexpectedResponse(resp)),
        }
    }

    pub async fn sign_tunnel_request(
        &self,
        request: TunnelRequest,
    ) -> Result<SignedRpcRequest<TunnelRequest>, ApiError> {
        match self
            .req(&AgentApiRequest::SignControlRequest(request))
            .await?
        {
            AgentApiResponse::SignedTunnelRequest(resp) => Ok(resp),
            resp => Err(ApiError::UnexpectedResponse(resp)),
        }
    }

    pub async fn generate_shared_tunnel_secret(
        &self,
        registered: AgentRegistered,
    ) -> Result<SessionSecret, ApiError> {
        match self
            .req(&AgentApiRequest::GenerateSharedTunnelSecret(registered))
            .await?
        {
            AgentApiResponse::SessionSecret(resp) => Ok(resp),
            resp => Err(ApiError::UnexpectedResponse(resp)),
        }
    }

    pub async fn try_exchange_claim_for_secret(&self, claim_url: &str) -> Result<Option<String>, ApiError> {
        let res = self.req(&AgentApiRequest::ExchangeClaimForSecret(ExchangeClaimForSecret {
            claim_key: claim_url.to_string()
        })).await;

        match res {
            Ok(AgentApiResponse::AgentSecret(secret)) => Ok(Some(secret.secret_key)),
            Ok(other) => Err(ApiError::UnexpectedResponse(other)),
            Err(ApiError::HttpError(404, _)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn get_agent_config(&self) -> Result<AgentConfig, ApiError> {
        let res = self.req(&AgentApiRequest::GetAgentConfig).await;

        match res {
            Ok(AgentApiResponse::AgentConfig(config)) => Ok(config),
            Ok(other) => Err(ApiError::UnexpectedResponse(other)),
            Err(error) => Err(error),
        }
    }

    async fn req(&self, req: &AgentApiRequest) -> Result<AgentApiResponse, ApiError> {
        let mut builder = Request::builder()
            .uri(&self.api_base)
            .method(Method::POST);

        if let Some(secret) = &self.agent_secret {
            builder = builder.header(
                hyper::header::AUTHORIZATION,
                format!("agent-key {}", secret),
            );
        }

        let request = builder
            .body(Body::from(serde_json::to_vec(req).unwrap()))
            .unwrap();

        let response = self.client.request(request).await?;
        let bytes = hyper::body::aggregate(response.into_body()).await?;

        let result = match serde_json::from_slice::<Response>(bytes.chunk())
        {
            Ok(v) => v,
            Err(error) => {
                let content = String::from_utf8_lossy(bytes.chunk());
                tracing::error!(?error, %content, "failed to parse response");
                return Err(ApiError::ParseError(error));
            }
        };

        match result {
            Response::Error(MiscError::Error { code, message }) => {
                Err(ApiError::HttpError(code, message))
            }
            Response::Ok(v) => Ok(v),
        }
    }
}

#[derive(Debug)]
pub enum ApiError {
    HttpError(u16, String),
    ParseError(serde_json::Error),
    RequestError(hyper::Error),
    UnexpectedResponse(AgentApiResponse),
}

impl From<hyper::Error> for ApiError {
    fn from(error: hyper::Error) -> Self {
        ApiError::RequestError(error)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Response {
    Ok(AgentApiResponse),
    Error(MiscError),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum MiscError {
    #[serde(rename = "error")]
    Error { code: u16, message: String },
}
