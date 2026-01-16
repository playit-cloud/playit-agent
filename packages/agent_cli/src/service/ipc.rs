//! IPC protocol for communication between CLI and background service.
//!
//! Uses JSON messages delimited by newlines over local sockets.

use std::io;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::sync::Arc;

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, ToFsName, ToNsName,
    tokio::{Stream, prelude::*},
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::broadcast;

use crate::ui::tui_app::{
    AccountStatusInfo, AgentData, ConnectionStats, NoticeInfo, PendingTunnelInfo, TunnelInfo,
};

/// Error types for IPC operations
#[derive(Debug)]
pub enum IpcError {
    /// Another instance is already running
    AlreadyRunning,
    /// Failed to bind to socket
    BindFailed(io::Error),
    /// Failed to connect to socket
    ConnectionFailed(io::Error),
    /// IO error during communication
    IoError(io::Error),
    /// JSON serialization/deserialization error
    JsonError(serde_json::Error),
    /// Service is not running
    NotRunning,
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcError::AlreadyRunning => write!(f, "Another instance is already running"),
            IpcError::BindFailed(e) => write!(f, "Failed to bind to socket: {}", e),
            IpcError::ConnectionFailed(e) => write!(f, "Failed to connect to socket: {}", e),
            IpcError::IoError(e) => write!(f, "IO error: {}", e),
            IpcError::JsonError(e) => write!(f, "JSON error: {}", e),
            IpcError::NotRunning => write!(f, "Service is not running"),
        }
    }
}

impl std::error::Error for IpcError {}

impl From<io::Error> for IpcError {
    fn from(e: io::Error) -> Self {
        IpcError::IoError(e)
    }
}

impl From<serde_json::Error> for IpcError {
    fn from(e: serde_json::Error) -> Self {
        IpcError::JsonError(e)
    }
}

/// Request messages from CLI to service
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServiceRequest {
    /// Subscribe to all updates (agent_data, stats, logs)
    Subscribe,
    /// One-shot status query
    Status,
    /// Request service shutdown
    Stop,
}

/// Event/response messages from service to CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServiceEvent {
    /// Status response
    Status {
        running: bool,
        pid: u32,
        uptime_secs: u64,
    },
    /// Agent data update (tunnels, notices, account info)
    AgentData {
        version: String,
        agent_id: String,
        account_status: String,
        login_link: Option<String>,
        tunnels: Vec<TunnelInfoJson>,
        pending_tunnels: Vec<PendingTunnelInfoJson>,
        notices: Vec<NoticeInfoJson>,
        start_time: u64,
    },
    /// Connection stats update
    Stats {
        bytes_in: u64,
        bytes_out: u64,
        active_tcp: u32,
        active_udp: u32,
    },
    /// Log entry
    Log {
        level: String,
        target: String,
        message: String,
        timestamp: u64,
    },
    /// Acknowledgement (for stop command)
    Ack { success: bool },
    /// Error response
    Error { message: String },
}

/// JSON-serializable tunnel info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelInfoJson {
    pub display_address: String,
    pub destination: String,
    pub is_disabled: bool,
    pub disabled_reason: Option<String>,
}

/// JSON-serializable pending tunnel info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTunnelInfoJson {
    pub id: String,
    pub status_msg: String,
}

/// JSON-serializable notice info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoticeInfoJson {
    pub priority: String,
    pub message: String,
    pub resolve_link: Option<String>,
}

impl From<&TunnelInfo> for TunnelInfoJson {
    fn from(t: &TunnelInfo) -> Self {
        TunnelInfoJson {
            display_address: t.display_address.clone(),
            destination: t.destination.clone(),
            is_disabled: t.is_disabled,
            disabled_reason: t.disabled_reason.clone(),
        }
    }
}

impl From<TunnelInfoJson> for TunnelInfo {
    fn from(t: TunnelInfoJson) -> Self {
        TunnelInfo {
            display_address: t.display_address,
            destination: t.destination,
            is_disabled: t.is_disabled,
            disabled_reason: t.disabled_reason,
        }
    }
}

impl From<&PendingTunnelInfo> for PendingTunnelInfoJson {
    fn from(p: &PendingTunnelInfo) -> Self {
        PendingTunnelInfoJson {
            id: p.id.clone(),
            status_msg: p.status_msg.clone(),
        }
    }
}

impl From<PendingTunnelInfoJson> for PendingTunnelInfo {
    fn from(p: PendingTunnelInfoJson) -> Self {
        PendingTunnelInfo {
            id: p.id,
            status_msg: p.status_msg,
        }
    }
}

impl From<&NoticeInfo> for NoticeInfoJson {
    fn from(n: &NoticeInfo) -> Self {
        NoticeInfoJson {
            priority: n.priority.clone(),
            message: n.message.clone(),
            resolve_link: n.resolve_link.clone(),
        }
    }
}

impl From<NoticeInfoJson> for NoticeInfo {
    fn from(n: NoticeInfoJson) -> Self {
        NoticeInfo {
            priority: n.priority,
            message: n.message,
            resolve_link: n.resolve_link,
        }
    }
}

impl From<&AgentData> for ServiceEvent {
    fn from(data: &AgentData) -> Self {
        ServiceEvent::AgentData {
            version: data.version.clone(),
            agent_id: data.agent_id.clone(),
            account_status: format!("{:?}", data.account_status),
            login_link: data.login_link.clone(),
            tunnels: data.tunnels.iter().map(|t| t.into()).collect(),
            pending_tunnels: data.pending_tunnels.iter().map(|p| p.into()).collect(),
            notices: data.notices.iter().map(|n| n.into()).collect(),
            start_time: data.start_time,
        }
    }
}

impl From<&ConnectionStats> for ServiceEvent {
    fn from(stats: &ConnectionStats) -> Self {
        ServiceEvent::Stats {
            bytes_in: stats.bytes_in,
            bytes_out: stats.bytes_out,
            active_tcp: stats.active_tcp,
            active_udp: stats.active_udp,
        }
    }
}

impl ServiceEvent {
    /// Convert AgentData event back to AgentData struct
    pub fn to_agent_data(&self) -> Option<AgentData> {
        match self {
            ServiceEvent::AgentData {
                version,
                agent_id,
                account_status,
                login_link,
                tunnels,
                pending_tunnels,
                notices,
                start_time,
            } => {
                let status = match account_status.as_str() {
                    "Guest" => AccountStatusInfo::Guest,
                    "EmailNotVerified" => AccountStatusInfo::EmailNotVerified,
                    "Verified" => AccountStatusInfo::Verified,
                    _ => AccountStatusInfo::Unknown,
                };
                Some(AgentData {
                    version: version.clone(),
                    agent_id: agent_id.clone(),
                    account_status: status,
                    login_link: login_link.clone(),
                    tunnels: tunnels.iter().cloned().map(|t| t.into()).collect(),
                    pending_tunnels: pending_tunnels.iter().cloned().map(|p| p.into()).collect(),
                    notices: notices.iter().cloned().map(|n| n.into()).collect(),
                    start_time: *start_time,
                })
            }
            _ => None,
        }
    }

    /// Convert Stats event back to ConnectionStats struct
    pub fn to_connection_stats(&self) -> Option<ConnectionStats> {
        match self {
            ServiceEvent::Stats {
                bytes_in,
                bytes_out,
                active_tcp,
                active_udp,
            } => Some(ConnectionStats {
                bytes_in: *bytes_in,
                bytes_out: *bytes_out,
                active_tcp: *active_tcp,
                active_udp: *active_udp,
            }),
            _ => None,
        }
    }
}

/// Get the socket path for the IPC connection
pub fn get_socket_path(system_mode: bool) -> String {
    // On Linux, only system-level service is supported (via package manager's systemd unit)
    #[cfg(target_os = "linux")]
    {
        let _ = system_mode; // silence unused variable warning - always uses system path
        "/var/run/playit-agent.sock".to_string()
    }

    #[cfg(target_os = "macos")]
    {
        if system_mode {
            "/var/run/playit-agent.sock".to_string()
        } else {
            let data_dir = dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("playit_gg");
            let _ = std::fs::create_dir_all(&data_dir);
            data_dir
                .join("playit-agent.sock")
                .to_string_lossy()
                .to_string()
        }
    }

    #[cfg(target_os = "windows")]
    {
        if system_mode {
            r"\\.\pipe\playit-agent-system".to_string()
        } else {
            format!(r"\\.\pipe\playit-agent-{}", whoami::username())
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = system_mode;
        "./playit-agent.sock".to_string()
    }
}

/// Check if another instance is running by attempting to connect
pub async fn is_instance_running(system_mode: bool) -> bool {
    let socket_path = get_socket_path(system_mode);
    try_connect(&socket_path).await.is_ok()
}

/// Try to connect to a socket path
async fn try_connect(socket_path: &str) -> Result<Stream, IpcError> {
    // Try namespaced socket first (for abstract sockets on Linux)
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

/// IPC Server for the background service
pub struct IpcServer {
    event_tx: broadcast::Sender<ServiceEvent>,
    socket_path: String,
    #[allow(dead_code)]
    system_mode: bool,
    start_time: u64,
    cancel_token: tokio_util::sync::CancellationToken,
}

impl IpcServer {
    /// Create a new IPC server
    ///
    /// This will fail if another instance is already running (single-instance enforcement)
    pub async fn new(
        system_mode: bool,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<Self, IpcError> {
        let (event_tx, _) = broadcast::channel(256);
        Self::new_with_sender(system_mode, cancel_token, event_tx).await
    }

    /// Create a new IPC server with an existing broadcast sender
    ///
    /// This allows sharing the event channel with other components (like logging)
    pub async fn new_with_sender(
        system_mode: bool,
        cancel_token: tokio_util::sync::CancellationToken,
        event_tx: broadcast::Sender<ServiceEvent>,
    ) -> Result<Self, IpcError> {
        use playit_agent_core::utils::now_milli;

        let socket_path = get_socket_path(system_mode);

        // Check if another instance is running
        if try_connect(&socket_path).await.is_ok() {
            return Err(IpcError::AlreadyRunning);
        }

        // Remove stale socket file if it exists (not needed for abstract sockets)
        if !socket_path.starts_with('@') && !socket_path.starts_with(r"\\.\pipe\") {
            let _ = std::fs::remove_file(&socket_path);
        }

        let start_time = now_milli();

        Ok(IpcServer {
            event_tx,
            socket_path,
            system_mode,
            start_time,
            cancel_token,
        })
    }

    /// Get a sender for broadcasting events to subscribers
    pub fn event_sender(&self) -> broadcast::Sender<ServiceEvent> {
        self.event_tx.clone()
    }

    /// Run the IPC server accept loop
    pub async fn run(self: Arc<Self>) -> Result<(), IpcError> {
        let listener = self.create_listener().await?;

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok(stream) => {
                            let server = self.clone();
                            tokio::spawn(async move {
                                if let Err(e) = server.handle_client(stream).await {
                                    tracing::warn!("Client connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!("Accept error: {}", e);
                        }
                    }
                }
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("IPC server shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    async fn create_listener(
        &self,
    ) -> Result<interprocess::local_socket::tokio::Listener, IpcError> {
        if self.socket_path.starts_with('@') {
            let name = self.socket_path[1..]
                .to_ns_name::<GenericNamespaced>()
                .map_err(|e| {
                    IpcError::BindFailed(io::Error::new(io::ErrorKind::InvalidInput, e))
                })?;
            ListenerOptions::new()
                .name(name)
                .create_tokio()
                .map_err(IpcError::BindFailed)
        } else {
            let name = self
                .socket_path
                .clone()
                .to_fs_name::<GenericFilePath>()
                .map_err(|e| {
                    IpcError::BindFailed(io::Error::new(io::ErrorKind::InvalidInput, e))
                })?;
            ListenerOptions::new()
                .name(name)
                .create_tokio()
                .map_err(IpcError::BindFailed)
        }
    }

    async fn handle_client(&self, stream: Stream) -> Result<(), IpcError> {
        let (reader, writer) = stream.split();
        let mut reader = BufReader::new(reader);
        let mut writer = BufWriter::new(writer);
        let mut line = String::new();
        let mut event_rx = self.event_tx.subscribe();

        loop {
            tokio::select! {
                // Read requests from client
                read_result = reader.read_line(&mut line) => {
                    match read_result {
                        Ok(0) => break, // Connection closed
                        Ok(_) => {
                            let request: ServiceRequest = serde_json::from_str(line.trim())?;
                            line.clear();

                            match request {
                                ServiceRequest::Subscribe => {
                                    // Client wants to receive events - handled by event_rx
                                    tracing::debug!("Client subscribed to events");
                                }
                                ServiceRequest::Status => {
                                    use playit_agent_core::utils::now_milli;
                                    let uptime_ms = now_milli().saturating_sub(self.start_time);
                                    let uptime_secs = uptime_ms / 1000;
                                    let event = ServiceEvent::Status {
                                        running: true,
                                        pid: std::process::id(),
                                        uptime_secs,
                                    };
                                    self.send_event(&mut writer, &event).await?;
                                }
                                ServiceRequest::Stop => {
                                    self.send_event(&mut writer, &ServiceEvent::Ack { success: true }).await?;
                                    tracing::info!("Stop request received, initiating shutdown");
                                    // Trigger daemon shutdown
                                    self.cancel_token.cancel();
                                }
                            }
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                // Forward events to client
                event_result = event_rx.recv() => {
                    match event_result {
                        Ok(event) => {
                            self.send_event(&mut writer, &event).await?;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            // Client is too slow, skip some events
                            tracing::warn!("Client lagged behind, some events dropped");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn send_event<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &mut BufWriter<W>,
        event: &ServiceEvent,
    ) -> Result<(), IpcError> {
        let json = serde_json::to_string(event)?;
        writer.write_all(json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }
}

/// IPC Client for connecting to the background service
pub struct IpcClient {
    reader: BufReader<interprocess::local_socket::tokio::RecvHalf>,
    writer: BufWriter<interprocess::local_socket::tokio::SendHalf>,
}

impl IpcClient {
    /// Connect to the background service
    pub async fn connect(system_mode: bool) -> Result<Self, IpcError> {
        let socket_path = get_socket_path(system_mode);
        let stream = try_connect(&socket_path).await?;
        let (reader, writer) = stream.split();

        Ok(IpcClient {
            reader: BufReader::new(reader),
            writer: BufWriter::new(writer),
        })
    }

    /// Check if the service is running (without maintaining connection)
    pub async fn is_running(system_mode: bool) -> bool {
        is_instance_running(system_mode).await
    }

    /// Send a request to the service
    pub async fn send_request(&mut self, request: &ServiceRequest) -> Result<(), IpcError> {
        let json = serde_json::to_string(request)?;
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Receive an event from the service
    pub async fn recv_event(&mut self) -> Result<ServiceEvent, IpcError> {
        let mut line = String::new();
        let bytes_read = self.reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Err(IpcError::IoError(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Connection closed",
            )));
        }
        let event = serde_json::from_str(line.trim())?;
        Ok(event)
    }

    /// Subscribe to events and return a stream of events
    pub async fn subscribe(&mut self) -> Result<(), IpcError> {
        self.send_request(&ServiceRequest::Subscribe).await
    }

    /// Request service status
    pub async fn status(&mut self) -> Result<ServiceEvent, IpcError> {
        self.send_request(&ServiceRequest::Status).await?;
        self.recv_event().await
    }

    /// Request service stop
    pub async fn stop(&mut self) -> Result<ServiceEvent, IpcError> {
        self.send_request(&ServiceRequest::Stop).await?;
        self.recv_event().await
    }
}
