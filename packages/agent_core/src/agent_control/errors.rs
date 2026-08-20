use std::{
    error::Error,
    fmt::{Display, Formatter},
    future::Future,
    net::SocketAddr,
    panic::Location,
    time::Duration,
};

use crate::gateway::GatewayError;
use futures_util::TryFutureExt;

#[derive(Debug)]
pub enum SetupError {
    IoError(std::io::Error),
    FailedToConnect,
    Gateway(GatewayError),
    AttemptingToAuthWithOldFlow,
    FailedToDecodeSignedAgentRegisterHex,
    NoResponseFromAuthenticate,
    RegisterInvalidSignature,
    RegisterUnauthorized,
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

impl From<GatewayError> for SetupError {
    fn from(value: GatewayError) -> Self {
        Self::Gateway(value)
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
    InvalidRemote {
        expected: SocketAddr,
        got: SocketAddr,
    },
    FailedToReadControlFeed(std::io::Error),
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
