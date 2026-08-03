use playit_agent_core::agent_control::errors::SetupError;
use playit_ipc::ipc::IpcError;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("IPC error: {0}")]
    Ipc(#[from] IpcError),
    #[error("Secret error: {0}")]
    Secret(#[from] SecretError),
    #[error("Logging error: {0}")]
    Logging(#[from] LoggingError),
    #[error("Agent setup error: {0}")]
    Agent(#[from] SetupError),
    #[error("Setup error: {0}")]
    Runtime(String),
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct SecretError(pub String);

impl From<String> for SecretError {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretError {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct LoggingError(pub String);

impl From<String> for LoggingError {
    fn from(value: String) -> Self {
        Self(value)
    }
}
