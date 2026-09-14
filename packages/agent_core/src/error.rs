use std::fmt::{Display, Formatter};

use playit_api_client::{
    api::{ApiError, ApiErrorNoFail, ApiResponseError},
    http_client::HttpClientError,
};

/// Failures while establishing (or re-establishing) the control connection.
#[derive(Debug)]
pub enum SetupError {
    Io(std::io::Error),
    Api(ApiResponseError),
    ApiFail(String),
    Http(HttpClientError),
    /// The API returned no control addresses to connect to.
    NoControlAddresses,
    /// None of the control addresses answered a ping.
    NoControlResponse,
    /// The signed register key returned by the API was not valid hex.
    InvalidSignedKey,
    RegisterInvalidSignature,
    /// The tunnel server rejected the registration. Usually the client address the
    /// API signed no longer matches what the tunnel server sees.
    RegisterUnauthorized,
    /// The tunnel server never answered the register request.
    RegisterNoResponse,
    /// The addresses seen by the tunnel server changed while registering.
    AddressChanged,
    Timeout(&'static str),
}

impl Display for SetupError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::Io(error) => write!(f, "io error: {error}"),
            SetupError::Api(error) => write!(f, "api error: {error:?}"),
            SetupError::ApiFail(detail) => write!(f, "api request failed: {detail}"),
            SetupError::Http(error) => write!(f, "http error: {error:?}"),
            SetupError::NoControlAddresses => write!(f, "api returned no control addresses"),
            SetupError::NoControlResponse => write!(f, "no control address responded"),
            SetupError::InvalidSignedKey => write!(f, "signed register key is not valid hex"),
            SetupError::RegisterInvalidSignature => write!(f, "register signature rejected"),
            SetupError::RegisterUnauthorized => write!(f, "register request unauthorized"),
            SetupError::RegisterNoResponse => write!(f, "no response to register request"),
            SetupError::AddressChanged => write!(f, "observed address changed during register"),
            SetupError::Timeout(stage) => write!(f, "timeout during {stage}"),
        }
    }
}

impl std::error::Error for SetupError {}

impl From<std::io::Error> for SetupError {
    fn from(error: std::io::Error) -> Self {
        SetupError::Io(error)
    }
}

impl<F: serde::Serialize> From<ApiError<F, HttpClientError>> for SetupError {
    fn from(error: ApiError<F, HttpClientError>) -> Self {
        match error {
            ApiError::ApiError(api) => SetupError::Api(api),
            ApiError::ClientError(http) => SetupError::Http(http),
            ApiError::Fail(fail) => SetupError::ApiFail(
                serde_json::to_string(&fail).unwrap_or_else(|_| "unserializable".to_owned()),
            ),
        }
    }
}

impl From<ApiErrorNoFail<HttpClientError>> for SetupError {
    fn from(error: ApiErrorNoFail<HttpClientError>) -> Self {
        match error {
            ApiErrorNoFail::ApiError(api) => SetupError::Api(api),
            ApiErrorNoFail::ClientError(http) => SetupError::Http(http),
        }
    }
}
