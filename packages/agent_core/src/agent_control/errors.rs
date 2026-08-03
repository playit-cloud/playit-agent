use std::{future::Future, net::SocketAddr, panic::Location, time::Duration};

use futures_util::TryFutureExt;
use playit_api_client::{
    api::{AgentRoutingGetError, ApiError, ApiErrorNoFail, ApiResponseError, ProtoRegisterError},
    http_client::HttpClientError,
};

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("network I/O failed: {0}")]
    IoError(std::io::Error),
    #[error("failed to connect to a tunnel control server")]
    FailedToConnect,
    #[error("agent registration was rejected: {0}")]
    ApiFail(ProtoRegisterError),
    #[error("API request was rejected: {0}")]
    RawApiFail(String),
    #[error("API returned an error: {0}")]
    ApiError(ApiResponseError),
    #[error("API request failed: {0:?}")]
    RequestError(HttpClientError),
    #[error("attempted to authenticate with the retired protocol")]
    AttemptingToAuthWithOldFlow,
    #[error("failed to decode the signed agent registration")]
    FailedToDecodeSignedAgentRegisterHex,
    #[error("authentication did not return a response")]
    NoResponseFromAuthenticate,
    #[error("agent registration signature was invalid")]
    RegisterInvalidSignature,
    #[error("agent registration was unauthorized")]
    RegisterUnauthorized,
    #[error("operation timed out at {0}")]
    Timeout(TimeoutSource),
}

impl From<TimeoutSource> for SetupError {
    fn from(value: TimeoutSource) -> Self {
        SetupError::Timeout(value)
    }
}

#[derive(Debug)]
pub struct TimeoutSource {
    pub file_name: &'static str,
    pub line_no: u32,
}

impl std::fmt::Display for TimeoutSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.file_name, self.line_no)
    }
}

impl TimeoutSource {
    pub fn from_location(location: &'static Location<'static>) -> Self {
        TimeoutSource {
            file_name: location.file(),
            line_no: location.line(),
        }
    }
}

pub trait TimeoutHelper {
    type Data;

    fn timeout(self, max: Duration) -> impl Future<Output = Result<Self::Data, SetupError>>;
}

pub trait TryTimeoutHelper {
    type Success;
    type Error;

    fn try_timeout(self, max: Duration)
    -> impl Future<Output = Result<Self::Success, Self::Error>>;
}

impl<F: Future> TimeoutHelper for F {
    type Data = F::Output;

    #[track_caller]
    fn timeout(self, max: Duration) -> impl Future<Output = Result<Self::Data, SetupError>> {
        tokio::time::timeout(max, self)
            .map_err(|_| SetupError::Timeout(TimeoutSource::from_location(Location::caller())))
    }
}

impl<R, E: From<TimeoutSource>, F: Future<Output = Result<R, E>>> TryTimeoutHelper for F {
    type Success = R;
    type Error = E;

    #[track_caller]
    fn try_timeout(
        self,
        max: Duration,
    ) -> impl Future<Output = Result<Self::Success, Self::Error>> {
        let fut = tokio::time::timeout(max, self)
            .map_err(|_| E::from(TimeoutSource::from_location(Location::caller())));

        async {
            match fut.await {
                Ok(Ok(res)) => Ok(res),
                Err(err) | Ok(Err(err)) => Err(err),
            }
        }
    }
}

impl From<ApiError<ProtoRegisterError, HttpClientError>> for SetupError {
    fn from(value: ApiError<ProtoRegisterError, HttpClientError>) -> Self {
        match value {
            ApiError::ApiError(api) => SetupError::ApiError(api),
            ApiError::ClientError(error) => SetupError::RequestError(error),
            ApiError::Fail(fail) => SetupError::ApiFail(fail),
        }
    }
}

impl From<ApiError<AgentRoutingGetError, HttpClientError>> for SetupError {
    fn from(value: ApiError<AgentRoutingGetError, HttpClientError>) -> Self {
        match value {
            ApiError::ApiError(api) => SetupError::ApiError(api),
            ApiError::ClientError(error) => SetupError::RequestError(error),
            ApiError::Fail(fail) => SetupError::RawApiFail(
                serde_json::to_string(&fail)
                    .unwrap_or_else(|_| format!("unserializable API failure: {fail:?}")),
            ),
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

impl From<std::io::Error> for SetupError {
    fn from(e: std::io::Error) -> Self {
        SetupError::IoError(e)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("control I/O failed: {0}")]
    IoError(std::io::Error),
    #[error("received control packet from {got}, expected {expected}")]
    InvalidRemote {
        expected: SocketAddr,
        got: SocketAddr,
    },
    #[error("failed to decode control feed: {0}")]
    FailedToReadControlFeed(std::io::Error),
    #[error("control operation timed out at {0}")]
    Timeout(TimeoutSource),
}

impl From<std::io::Error> for ControlError {
    fn from(e: std::io::Error) -> Self {
        ControlError::IoError(e)
    }
}

impl From<TimeoutSource> for ControlError {
    fn from(value: TimeoutSource) -> Self {
        ControlError::Timeout(value)
    }
}
