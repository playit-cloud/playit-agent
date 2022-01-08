use std::net::SocketAddr;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use messages::api::{AgentApiRequest, AgentApiResponse, SessionSecret};
use messages::rpc::SignedRpcRequest;
use messages::{AgentRegistered, TunnelRequest};

pub struct ApiClient {
    api_base: String,
    agent_secret: Option<String>,
    request: Client,
}

impl ApiClient {
    pub fn new(api_base: String, agent_secret: Option<String>) -> Self {
        ApiClient {
            api_base,
            agent_secret,
            request: Client::new(),
        }
    }

    pub async fn get_control_addr(&self) -> Result<SocketAddr, ApiError> {
        match self.req(&AgentApiRequest::GetControlAddr).await? {
            AgentApiResponse::ControlAddress(addr) => Ok(addr.control_address),
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

    async fn req(&self, req: &AgentApiRequest) -> Result<AgentApiResponse, ApiError> {
        let mut builder = self.request.post(&self.api_base);
        if let Some(secret) = &self.agent_secret {
            builder = builder.header(
                reqwest::header::AUTHORIZATION,
                format!("agent-key {}", secret),
            );
        }

        let bytes = builder.json(req).send().await?.bytes().await?;

        let result = match serde_json::from_slice::<Response>(bytes.as_ref()) {
            Ok(v) => v,
            Err(error) => {
                let content = String::from_utf8_lossy(bytes.as_ref());
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
    RequestError(reqwest::Error),
    UnexpectedResponse(AgentApiResponse),
}

impl From<reqwest::Error> for ApiError {
    fn from(error: reqwest::Error) -> Self {
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
