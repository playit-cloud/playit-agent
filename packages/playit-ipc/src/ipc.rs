//! IPC protocol for communication between CLI and background service.

use std::io;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::sync::LazyLock;

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ToFsName, ToNsName,
    tokio::{Stream, prelude::*},
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

use crate::model::{
    AccountLoginUrlResponse, AgentLifecycle, CommandResponse, ProtocolInfo, SecretPathResponse,
    ServiceCapability, ServiceError, ServiceStatus, ServiceUpdate, SubscribeResponse,
};

pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Debug)]
pub enum IpcError {
    AlreadyRunning,
    BindFailed(io::Error),
    ConnectionFailed(io::Error),
    IoError(io::Error),
    JsonError(serde_json::Error),
    NotRunning,
    ProtocolMismatch { expected: u32, actual: u32 },
    ProtocolError(String),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "Another instance is already running"),
            Self::BindFailed(e) => write!(f, "Failed to bind to socket: {e}"),
            Self::ConnectionFailed(e) => write!(f, "Failed to connect to socket: {e}"),
            Self::IoError(e) => write!(f, "IO error: {e}"),
            Self::JsonError(e) => write!(f, "JSON error: {e}"),
            Self::NotRunning => write!(f, "Service is not running"),
            Self::ProtocolMismatch { expected, actual } => {
                write!(
                    f,
                    "IPC protocol mismatch: expected version {expected}, got {actual}"
                )
            }
            Self::ProtocolError(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for IpcError {}

impl From<io::Error> for IpcError {
    fn from(e: io::Error) -> Self {
        Self::IoError(e)
    }
}

impl From<serde_json::Error> for IpcError {
    fn from(e: serde_json::Error) -> Self {
        Self::JsonError(e)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub protocol_version: u32,
    pub request_id: u64,
    pub request: ServiceRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServiceRequest {
    Subscribe,
    GetStatus,
    GetState,
    Stop,
    SetSecret { secret: String },
    ResetSecret,
    GetSecretPath,
    GetAccountLoginUrl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub protocol_version: u32,
    pub request_id: u64,
    pub response: ServiceResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub protocol_version: u32,
    pub event: ServiceUpdate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServiceResponse {
    Subscribe(SubscribeResponse),
    Status(ServiceStatus),
    State(AgentLifecycle),
    Stop(CommandResponse),
    SetSecret(CommandResponse),
    ResetSecret(CommandResponse),
    SecretPath(SecretPathResponse),
    AccountLoginUrl(AccountLoginUrlResponse),
    Error(ServiceError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "message_kind", content = "data", rename_all = "snake_case")]
pub enum ServerEnvelope {
    Response(ResponseEnvelope),
    Event(EventEnvelope),
}

pub fn protocol_info() -> ProtocolInfo {
    ProtocolInfo {
        version: PROTOCOL_VERSION,
        capabilities: vec![
            ServiceCapability::StructuredResponses,
            ServiceCapability::StreamEvents,
            ServiceCapability::LifecycleState,
            ServiceCapability::RichStatus,
            ServiceCapability::SecretProvisioning,
        ],
    }
}

pub fn get_default_socket_path() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "/var/run/playitd.sock"
    }

    #[cfg(target_os = "macos")]
    {
        MACOS_DEFAULT_SOCKET_PATH.as_str()
    }

    #[cfg(target_os = "windows")]
    {
        r"\\.\pipe\playitd-system"
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "./playitd.sock"
    }
}

#[cfg(target_os = "macos")]
static MACOS_DEFAULT_SOCKET_PATH: LazyLock<String> =
    LazyLock::new(|| macos_launch_agent_socket_path().display().to_string());

#[cfg(target_os = "macos")]
pub fn macos_launch_agent_socket_path() -> PathBuf {
    dirs::config_local_dir()
        .unwrap_or_else(|| ".".into())
        .join("playit_gg")
        .join("playitd.sock")
}

async fn try_connect(socket_path: &str) -> Result<Stream, IpcError> {
    if socket_path.starts_with('@') {
        let name = socket_path[1..]
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| {
                IpcError::ConnectionFailed(io::Error::new(io::ErrorKind::InvalidInput, e))
            })?;
        Stream::connect(name)
            .await
            .map_err(IpcError::ConnectionFailed)
    } else {
        let name = socket_path.to_fs_name::<GenericFilePath>().map_err(|e| {
            IpcError::ConnectionFailed(io::Error::new(io::ErrorKind::InvalidInput, e))
        })?;
        Stream::connect(name)
            .await
            .map_err(IpcError::ConnectionFailed)
    }
}

pub struct IpcClient {
    reader: BufReader<interprocess::local_socket::tokio::RecvHalf>,
    writer: BufWriter<interprocess::local_socket::tokio::SendHalf>,
    next_request_id: u64,
}

impl IpcClient {
    pub async fn connect() -> Result<Self, IpcError> {
        Self::connect_with_path(get_default_socket_path()).await
    }

    pub async fn connect_with_path(socket_path: &str) -> Result<Self, IpcError> {
        let stream = try_connect(socket_path).await?;
        let (reader, writer) = stream.split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer: BufWriter::new(writer),
            next_request_id: 1,
        })
    }

    pub async fn is_running(socket_path: &str) -> bool {
        try_connect(socket_path).await.is_ok()
    }

    pub async fn subscribe(&mut self) -> Result<SubscribeResponse, IpcError> {
        match self.request(ServiceRequest::Subscribe).await? {
            ServiceResponse::Subscribe(response) => Ok(response),
            ServiceResponse::Error(error) => Err(IpcError::ProtocolError(error.to_string())),
            other => Err(IpcError::ProtocolError(format!(
                "expected subscribe response, got {other:?}"
            ))),
        }
    }

    pub async fn recv_update(&mut self) -> Result<ServiceUpdate, IpcError> {
        match self.recv_server_envelope().await? {
            ServerEnvelope::Event(event) => {
                self.ensure_protocol_version(event.protocol_version)?;
                Ok(event.event)
            }
            ServerEnvelope::Response(response) => Err(IpcError::ProtocolError(format!(
                "received RPC response while waiting for stream event: {:?}",
                response.response
            ))),
        }
    }

    pub async fn status(&mut self) -> Result<ServiceStatus, IpcError> {
        match self.request(ServiceRequest::GetStatus).await? {
            ServiceResponse::Status(status) => Ok(status),
            ServiceResponse::Error(error) => Err(IpcError::ProtocolError(error.to_string())),
            other => Err(IpcError::ProtocolError(format!(
                "expected status response, got {other:?}"
            ))),
        }
    }

    pub async fn lifecycle(&mut self) -> Result<AgentLifecycle, IpcError> {
        match self.request(ServiceRequest::GetState).await? {
            ServiceResponse::State(state) => Ok(state),
            ServiceResponse::Error(error) => Err(IpcError::ProtocolError(error.to_string())),
            other => Err(IpcError::ProtocolError(format!(
                "expected lifecycle response, got {other:?}"
            ))),
        }
    }

    pub async fn stop(&mut self) -> Result<CommandResponse, IpcError> {
        match self.request(ServiceRequest::Stop).await? {
            ServiceResponse::Stop(response) => Ok(response),
            ServiceResponse::Error(error) => Err(IpcError::ProtocolError(error.to_string())),
            other => Err(IpcError::ProtocolError(format!(
                "expected stop response, got {other:?}"
            ))),
        }
    }

    pub async fn set_secret(&mut self, secret: &str) -> Result<CommandResponse, IpcError> {
        match self
            .request(ServiceRequest::SetSecret {
                secret: secret.to_string(),
            })
            .await?
        {
            ServiceResponse::SetSecret(response) => Ok(response),
            ServiceResponse::Error(error) => Err(IpcError::ProtocolError(error.to_string())),
            other => Err(IpcError::ProtocolError(format!(
                "expected secret provisioning response, got {other:?}"
            ))),
        }
    }

    pub async fn reset_secret(&mut self) -> Result<CommandResponse, IpcError> {
        match self.request(ServiceRequest::ResetSecret).await? {
            ServiceResponse::ResetSecret(response) => Ok(response),
            ServiceResponse::Error(error) => Err(IpcError::ProtocolError(error.to_string())),
            other => Err(IpcError::ProtocolError(format!(
                "expected reset secret response, got {other:?}"
            ))),
        }
    }

    pub async fn get_secret_path(&mut self) -> Result<SecretPathResponse, IpcError> {
        match self.request(ServiceRequest::GetSecretPath).await? {
            ServiceResponse::SecretPath(response) => Ok(response),
            ServiceResponse::Error(error) => Err(IpcError::ProtocolError(error.to_string())),
            other => Err(IpcError::ProtocolError(format!(
                "expected secret path response, got {other:?}"
            ))),
        }
    }

    pub async fn get_account_login_url(&mut self) -> Result<AccountLoginUrlResponse, IpcError> {
        match self.request(ServiceRequest::GetAccountLoginUrl).await? {
            ServiceResponse::AccountLoginUrl(response) => Ok(response),
            ServiceResponse::Error(error) => Err(IpcError::ProtocolError(error.to_string())),
            other => Err(IpcError::ProtocolError(format!(
                "expected account login URL response, got {other:?}"
            ))),
        }
    }

    pub async fn request(&mut self, request: ServiceRequest) -> Result<ServiceResponse, IpcError> {
        let request_id = self.send_request(request).await?;
        let response = self.recv_response().await?;

        self.ensure_protocol_version(response.protocol_version)?;
        if response.request_id != request_id {
            return Err(IpcError::ProtocolError(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        Ok(response.response)
    }

    async fn send_request(&mut self, request: ServiceRequest) -> Result<u64, IpcError> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;

        let json = serde_json::to_string(&RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            request,
        })?;
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(request_id)
    }

    async fn recv_response(&mut self) -> Result<ResponseEnvelope, IpcError> {
        match self.recv_server_envelope().await? {
            ServerEnvelope::Response(response) => Ok(response),
            ServerEnvelope::Event(event) => Err(IpcError::ProtocolError(format!(
                "received stream event while waiting for RPC response: {:?}",
                event.event
            ))),
        }
    }

    async fn recv_server_envelope(&mut self) -> Result<ServerEnvelope, IpcError> {
        let mut line = String::new();
        let bytes_read = self.reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Err(IpcError::IoError(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Connection closed",
            )));
        }
        Ok(serde_json::from_str(line.trim())?)
    }

    fn ensure_protocol_version(&self, actual: u32) -> Result<(), IpcError> {
        if actual == PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(IpcError::ProtocolMismatch {
                expected: PROTOCOL_VERSION,
                actual,
            })
        }
    }
}
