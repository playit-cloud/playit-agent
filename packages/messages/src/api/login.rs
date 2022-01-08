use std::fmt::{Debug, Formatter};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
pub enum LoginApiRequest {
    #[serde(rename = "sign-in")]
    SignIn(LoginCredentials),

    #[serde(rename = "create-account")]
    CreateAccount(LoginCredentials),

    #[serde(rename = "refresh-session")]
    RefreshSession(RefreshSession),

    #[serde(rename = "create-discourse-session")]
    CreateDiscourseSession(CreateDiscourseSession),
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct LoginCredentials {
    pub email: String,
    pub password: String,
}

impl Debug for LoginCredentials {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LoginCredentials {{ email: {}, password: <redacted> }}",
            self.email
        )
    }
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct RefreshSession {
    pub expired_session_key: String,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct CreateDiscourseSession {
    pub nonce_payload: String,
    pub signature: String,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
pub enum LoginApiResponse {
    #[serde(rename = "signed-in")]
    SignedIn(WebSession),

    #[serde(rename = "discourse-session")]
    DiscourseSession(DiscourseSession),
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct WebSession {
    pub account_id: u64,
    pub session_key: String,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct DiscourseSession {
    pub payload: String,
    pub signature: String,
}
