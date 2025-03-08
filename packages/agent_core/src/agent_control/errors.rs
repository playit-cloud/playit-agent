use std::{error::Error, fmt::{Display, Formatter}, net::SocketAddr};

use playit_api_client::{api::{ApiError, ApiErrorNoFail, ApiResponseError}, http_client::HttpClientError};


#[derive(Debug)]
pub enum SetupError {
    IoError(std::io::Error),
    FailedToConnect,
    ApiFail(String),
    ApiError(ApiResponseError),
    RequestError(HttpClientError),
    AttemptingToAuthWithOldFlow,
    FailedToDecodeSignedAgentRegisterHex,
    NoResponseFromAuthenticate,
    RegisterInvalidSignature,
    RegisterUnauthorized,
}

impl<F: serde::Serialize> From<ApiError<F, HttpClientError>> for SetupError {
    fn from(value: ApiError<F, HttpClientError>) -> Self {
        match value {
            ApiError::ApiError(api) => SetupError::ApiError(api),
            ApiError::ClientError(error) => SetupError::RequestError(error),
            ApiError::Fail(fail) => SetupError::ApiFail(serde_json::to_string(&fail).unwrap())
        }
    }
}

impl From<ApiErrorNoFail<HttpClientError>> for SetupError {
    fn from(value: ApiErrorNoFail<HttpClientError>) -> Self {
        match value {
            ApiErrorNoFail::ApiError(api) => SetupError::ApiError(api),
            ApiErrorNoFail::ClientError(error) => SetupError::RequestError(error),
        }
    }
}

impl Display for SetupError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for SetupError {}

impl From<std::io::Error> for SetupError {
    fn from(e: std::io::Error) -> Self {
        SetupError::IoError(e)
    }
}

#[derive(Debug)]
pub enum ControlError {
    IoError(std::io::Error),
    InvalidRemote { expected: SocketAddr, got: SocketAddr },
    FailedToReadControlFeed(std::io::Error),
}

impl From<std::io::Error> for ControlError {
    fn from(e: std::io::Error) -> Self {
        ControlError::IoError(e)
    }
}
