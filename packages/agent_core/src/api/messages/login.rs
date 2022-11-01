use crate::api::messages::{ApiRequest, SimpleApiRequest};
use serde::{Serialize, Deserialize};
use uuid::Uuid;


#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum LoginApiRequest {
    #[serde(rename = "get-session")]
    GetSession,

    #[serde(rename = "create-guest-session")]
    CreateGuestSession,
}

pub struct GetSession;

impl ApiRequest for GetSession {
    type RequestJson = LoginApiRequest;
    type ResponseJson = LoginApiResponse;
    type Response = SessionStatus;

    fn to_req(self) -> Self::RequestJson {
        LoginApiRequest::GetSession
    }

    fn extract_response(parsed: Self::ResponseJson) -> Option<Self::Response> {
        match parsed {
            LoginApiResponse::SessionStatus(s) => Some(s),
            _ => None,
        }
    }

    fn endpoint() -> &'static str {
        "/login"
    }
}

pub struct CreateGuestSession;

impl ApiRequest for CreateGuestSession {
    type RequestJson = LoginApiRequest;
    type ResponseJson = LoginApiResponse;
    type Response = WebSession;

    fn to_req(self) -> Self::RequestJson {
        LoginApiRequest::CreateGuestSession
    }

    fn extract_response(parsed: Self::ResponseJson) -> Option<Self::Response> {
        match parsed {
            LoginApiResponse::SignedIn(session) => Some(session),
            _ => None,
        }
    }

    fn endpoint() -> &'static str {
        "/login"
    }
}

impl SimpleApiRequest for LoginApiRequest {
    type Response = LoginApiResponse;

    fn endpoint() -> &'static str {
        "/login"
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum LoginApiResponse {
    #[serde(rename = "signed-in")]
    SignedIn(WebSession),

    #[serde(rename = "session-status")]
    SessionStatus(SessionStatus),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct WebSession {
    pub account_id: u64,
    pub session_key: String,
    pub is_guest: bool,
    pub email_verified: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SessionStatus {
    pub account_id: u64,
    pub is_guest: bool,
    pub email_verified: bool,
    pub agent_id: Option<Uuid>,
    pub notice: Option<Notice>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Notice {
    pub url: String,
    pub message: String,
}