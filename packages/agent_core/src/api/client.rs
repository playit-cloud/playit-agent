use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};
use std::net::SocketAddr;

use hyper::{Body, header, Method, Request};
use hyper::body::Buf;
use hyper::client::HttpConnector;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use tracing::Instrument;
use crate::api::messages::*;


pub struct ApiClient {
    api_base: String,
    agent_secret: Option<String>,
    client: hyper::Client<HttpsConnector<HttpConnector>, Body>,
}

impl Debug for ApiClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApiClient{{ has_secret: {} }}", self.agent_secret.is_some())
    }
}

impl ApiClient {
    pub fn new(api_base: String, agent_secret: Option<String>) -> Self {
        let connector = if api_base.starts_with("http://") {
            HttpsConnectorBuilder::new()
                .with_native_roots()
                .https_or_http()
                .enable_http1()
                .enable_http2()
                .build()
        } else {
            HttpsConnectorBuilder::new()
                .with_native_roots()
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

    pub async fn sign_and_register(&self, details: SignAgentRegister) -> Result<SignedAgentRegister, ApiError> {
        self.req(details).await
    }

    pub async fn get_control_addr(&self) -> Result<SocketAddr, ApiError> {
        match self.req(AgentApiRequest::GetControlAddr).await? {
            AgentApiResponse::ControlAddress(addr) => Ok(addr.control_address),
            resp => Err(ApiError::UnexpectedResponse(format!("{:?}", resp))),
        }
    }

    pub async fn get_agent_account_status(&self) -> Result<AgentAccountStatus, ApiError> {
        match self.req(AgentApiRequest::GetAgentAccountStatus {
            client_version: Some(env!("CARGO_PKG_VERSION").to_string())
        }).await? {
            AgentApiResponse::AgentAccountStatus(status) => Ok(status),
            resp => Err(ApiError::UnexpectedResponse(format!("{:?}", resp))),
        }
    }

    pub async fn try_exchange_claim_for_secret(&self, claim_code: &str) -> Result<Option<String>, ApiError> {
        let res = self.req(AgentApiRequest::ExchangeClaimForSecret(ExchangeClaimForSecret {
            claim_key: claim_code.to_string()
        })).await;

        match res {
            Ok(AgentApiResponse::AgentSecret(secret)) => Ok(Some(secret.secret_key)),
            Ok(other) => Err(ApiError::UnexpectedResponse(format!("{:?}", other))),
            Err(ApiError::HttpError(404, _)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn req<T: ApiRequest>(&self, req: T) -> Result<T::Response, ApiError> where T::ResponseJson: DeserializeOwned, T::RequestJson: Serialize {
        println!("req: {}", std::any::type_name::<T>());
        let mut builder = Request::builder()
            .uri(format!("{}{}", self.api_base, T::endpoint()))
            .method(Method::POST);

        if let Some(secret) = &self.agent_secret {
            builder = builder.header(
                header::AUTHORIZATION,
                format!("agent-key {}", secret),
            );
        }

        let request_str = serde_json::to_string(&req.to_req()).unwrap();
        println!("req body: {}", request_str);
        let request = builder
            .body(Body::from(request_str))
            .unwrap();

        let response = self.client.request(request).await?;
        let bytes = hyper::body::aggregate(response.into_body()).await?;

        let result = match serde_json::from_slice::<Response<T::ResponseJson>>(bytes.chunk())
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
            Response::Ok(v) => match T::extract_response(v) {
                Some(v) => Ok(v),
                None => {
                    let content = String::from_utf8_lossy(bytes.chunk()).to_string();
                    Err(ApiError::UnexpectedResponse(content))
                }
            },
        }
    }
}

#[derive(Debug)]
pub enum ApiError {
    HttpError(u16, String),
    ParseError(serde_json::Error),
    RequestError(hyper::Error),
    UnexpectedResponse(String),
}

impl Display for ApiError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for ApiError {
}

impl From<hyper::Error> for ApiError {
    fn from(error: hyper::Error) -> Self {
        ApiError::RequestError(error)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Response<T> {
    Ok(T),
    Error(MiscError),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum MiscError {
    #[serde(rename = "error")]
    Error { code: u16, message: String },
}
