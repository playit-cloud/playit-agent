use std::fmt::{Debug, Formatter};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum LoginApiRequest {
    #[serde(rename = "sign-in")]
    SignIn(LoginCredentials),

    #[serde(rename = "create-account")]
    CreateAccount(LoginCredentials),

    #[serde(rename = "create-account-from-guest")]
    CreateAccountFromGuest(LoginCredentials),

    #[serde(rename = "create-guest-account")]
    CreateGuestAccount,

    #[serde(rename = "refresh-session")]
    RefreshSession(RefreshSession),

    #[serde(rename = "create-discourse-session")]
    CreateDiscourseSession(CreateDiscourseSession),

    #[serde(rename = "reset-password")]
    ResetPassword(ResetPassword),
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct LoginCredentials {
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ResetPassword {
    pub reset_code: String,
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

impl Debug for ResetPassword {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ResetPassword {{ reset_code: <redacted>, email: {}, password: <redacted> }}",
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
    pub is_guest: bool,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct DiscourseSession {
    pub payload: String,
    pub signature: String,
}
