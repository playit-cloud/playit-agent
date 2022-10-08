use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use crate::api::messages::{ApiRequest, SimpleApiRequest};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum AgentApiRequest {
    #[serde(rename = "get-control-address")]
    GetControlAddr,

    #[serde(rename = "get-agent-account-status")]
    GetAgentAccountStatus { client_version: Option<String> },

    #[serde(rename = "exchange-claim-for-secret")]
    ExchangeClaimForSecret(ExchangeClaimForSecret),

    #[serde(rename = "sign-agent-register")]
    SignAgentRegister(SignAgentRegister),
}

impl SimpleApiRequest for AgentApiRequest {
    type Response = AgentApiResponse;

    fn endpoint() -> &'static str {
        "/agent"
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SignAgentRegister {
    pub agent_version: u64,
    pub client_addr: SocketAddr,
    pub tunnel_addr: SocketAddr,
}

impl ApiRequest for SignAgentRegister {
    type RequestJson = AgentApiRequest;
    type ResponseJson = AgentApiResponse;
    type Response = SignedAgentRegister;

    fn to_req(self) -> Self::RequestJson {
        AgentApiRequest::SignAgentRegister(self)
    }

    fn extract_response(parsed: Self::ResponseJson) -> Option<Self::Response> {
        match parsed {
            AgentApiResponse::SignedAgentRegister(v) => Some(v),
            _ => None,
        }
    }

    fn endpoint() -> &'static str {
        "/agent"
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExchangeClaimForSecret {
    pub claim_key: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum AgentApiResponse {
    #[serde(rename = "control-address")]
    ControlAddress(ControlAddress),

    #[serde(rename = "agent-secret")]
    AgentSecret(AgentSecret),

    #[serde(rename = "agent-account-status")]
    AgentAccountStatus(AgentAccountStatus),

    #[serde(rename = "signed-agent-register")]
    SignedAgentRegister(SignedAgentRegister),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SignedAgentRegister {
    pub data: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ControlAddress {
    pub control_address: SocketAddr,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentSecret {
    pub secret_key: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "status")]
pub enum AgentAccountStatus {
    #[serde(rename = "no-account")]
    NoAccount {},
    #[serde(rename = "guest-account")]
    GuestAccount {
        account_id: u64,
        web_session_key: String,
    },
    #[serde(rename = "unverified-account")]
    UnverifiedAccount {
        account_id: u64
    },
    #[serde(rename = "verified-account")]
    VerifiedAccount {
        account_id: u64
    },
    #[serde(rename = "user-notice")]
    UserNotice {
        message: String,
        notice_url: String,
        important: bool,
        prevent_usage: bool,
    },
}
