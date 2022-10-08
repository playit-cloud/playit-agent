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