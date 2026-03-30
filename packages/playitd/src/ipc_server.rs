use std::path::Path;
use std::sync::Arc;

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, ToFsName, ToNsName,
    tokio::{Stream, prelude::*},
};
use playit_agent_core::utils::now_milli;
use playit_ipc::ipc::{
    EventEnvelope, IpcError, PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope, ServerEnvelope,
    ServiceRequest, ServiceResponse, protocol_info, resolve_socket_path,
};
use playit_ipc::model::{
    AgentLifecycle, CommandResponse, ConnectionStats, ServiceError, ServiceErrorCode,
    ServiceStatus, ServiceUpdate, SubscribeResponse, SubscriptionSnapshot,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct StateCache {
    lifecycle: RwLock<AgentLifecycle>,
    status: RwLock<ServiceStatus>,
    stats: RwLock<ConnectionStats>,
}

impl StateCache {
    pub async fn set_lifecycle(&self, lifecycle: AgentLifecycle) {
        *self.lifecycle.write().await = lifecycle;
    }

    pub async fn lifecycle(&self) -> AgentLifecycle {
        self.lifecycle.read().await.clone()
    }

    pub async fn set_status(&self, status: ServiceStatus) {
        *self.status.write().await = status;
    }

    pub async fn status(&self) -> ServiceStatus {
        self.status.read().await.clone()
    }

    pub async fn set_stats(&self, stats: ConnectionStats) {
        *self.stats.write().await = stats;
    }

    pub async fn stats(&self) -> ConnectionStats {
        self.stats.read().await.clone()
    }

    pub async fn subscription_snapshot(&self) -> SubscriptionSnapshot {
        SubscriptionSnapshot {
            status: self.status().await,
            lifecycle: self.lifecycle().await,
            stats: self.stats().await,
        }
    }
}

pub struct SecretProvisionRequest {
    pub secret: String,
    pub response_tx: oneshot::Sender<Result<(), String>>,
}

pub struct IpcServer {
    event_tx: broadcast::Sender<ServiceUpdate>,
    socket_path: String,
    start_time: u64,
    cancel_token: CancellationToken,
    state_cache: Arc<StateCache>,
    secret_provision_tx: Option<mpsc::Sender<SecretProvisionRequest>>,
}

impl IpcServer {
    pub async fn new_with_sender(
        system_mode: bool,
        socket_path: Option<String>,
        cancel_token: CancellationToken,
        event_tx: broadcast::Sender<ServiceUpdate>,
        secret_provision_tx: Option<mpsc::Sender<SecretProvisionRequest>>,
    ) -> Result<Self, IpcError> {
        let socket_path = resolve_socket_path(socket_path.as_deref(), system_mode);

        if try_connect(&socket_path).await.is_ok() {
            return Err(IpcError::AlreadyRunning);
        }

        if !socket_path.starts_with('@') && !socket_path.starts_with(r"\\.\pipe\") {
            if let Some(parent) = Path::new(&socket_path)
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::remove_file(&socket_path);
        }

        Ok(Self {
            event_tx,
            socket_path,
            start_time: now_milli(),
            cancel_token,
            state_cache: Arc::new(StateCache::default()),
            secret_provision_tx,
        })
    }

    pub fn event_sender(&self) -> broadcast::Sender<ServiceUpdate> {
        self.event_tx.clone()
    }

    pub fn state_cache(&self) -> Arc<StateCache> {
        self.state_cache.clone()
    }

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
                                    tracing::warn!("Client connection error: {e}");
                                }
                            });
                        }
                        Err(e) => tracing::error!("Accept error: {e}"),
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
                .map_err(|e| IpcError::BindFailed(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)))?;
            ListenerOptions::new()
                .name(name)
                .create_tokio()
                .map_err(IpcError::BindFailed)
        } else {
            let name = self
                .socket_path
                .clone()
                .to_fs_name::<GenericFilePath>()
                .map_err(|e| IpcError::BindFailed(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)))?;
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
        let mut subscribed = false;

        loop {
            tokio::select! {
                read_result = reader.read_line(&mut line) => {
                    match read_result {
                        Ok(0) => break,
                        Ok(_) => {
                            let request = serde_json::from_str::<RequestEnvelope>(line.trim())?;
                            line.clear();

                            if request.protocol_version != PROTOCOL_VERSION {
                                self.send_response(
                                    &mut writer,
                                    request.request_id,
                                    ServiceResponse::Error(protocol_error(
                                        ServiceErrorCode::UnsupportedProtocol,
                                        format!(
                                            "unsupported protocol version {} (expected {})",
                                            request.protocol_version,
                                            PROTOCOL_VERSION
                                        ),
                                        false,
                                    )),
                                ).await?;
                                continue;
                            }

                            match request.request {
                                ServiceRequest::Subscribe => {
                                    subscribed = true;
                                    let snapshot = self.state_cache.subscription_snapshot().await;
                                    self.send_response(
                                        &mut writer,
                                        request.request_id,
                                        ServiceResponse::Subscribe(SubscribeResponse {
                                            protocol: protocol_info(),
                                            snapshot,
                                        }),
                                    )
                                    .await?;
                                }
                                ServiceRequest::GetStatus => {
                                    let mut status = self.state_cache.status().await;
                                    let uptime_ms = now_milli().saturating_sub(self.start_time);
                                    status.uptime_secs = uptime_ms / 1000;
                                    self.send_response(
                                        &mut writer,
                                        request.request_id,
                                        ServiceResponse::Status(status),
                                    )
                                    .await?;
                                }
                                ServiceRequest::GetState => {
                                    self.send_response(
                                        &mut writer,
                                        request.request_id,
                                        ServiceResponse::State(self.state_cache.lifecycle().await),
                                    )
                                    .await?;
                                }
                                ServiceRequest::Stop => {
                                    tracing::info!("Stop request received, initiating shutdown");
                                    self.cancel_token.cancel();
                                    self.send_response(
                                        &mut writer,
                                        request.request_id,
                                        ServiceResponse::Stop(CommandResponse {
                                            accepted: true,
                                            message: Some("shutdown requested".to_string()),
                                        }),
                                    )
                                    .await?;
                                }
                                ServiceRequest::SetSecret { secret } => {
                                    let Some(secret_provision_tx) = &self.secret_provision_tx else {
                                        self.send_response(
                                            &mut writer,
                                            request.request_id,
                                            ServiceResponse::Error(protocol_error(
                                                ServiceErrorCode::ProvisioningUnavailable,
                                                "Secret provisioning is unavailable".to_string(),
                                                true,
                                            )),
                                        )
                                        .await?;
                                        continue;
                                    };

                                    let (response_tx, response_rx) = oneshot::channel();
                                    secret_provision_tx
                                        .send(SecretProvisionRequest { secret, response_tx })
                                        .await
                                        .map_err(|_| IpcError::ProtocolError(
                                            "daemon cannot accept secret provisioning right now".to_string()
                                        ))?;

                                    match response_rx.await {
                                        Ok(Ok(())) => {
                                            self.send_response(
                                                &mut writer,
                                                request.request_id,
                                                ServiceResponse::SetSecret(CommandResponse {
                                                    accepted: true,
                                                    message: Some("secret provisioned".to_string()),
                                                }),
                                            )
                                            .await?;
                                        }
                                        Ok(Err(message)) => {
                                            self.send_response(
                                                &mut writer,
                                                request.request_id,
                                                ServiceResponse::Error(protocol_error(
                                                    ServiceErrorCode::ConfigWriteFailed,
                                                    message,
                                                    true,
                                                )),
                                            )
                                            .await?;
                                        }
                                        Err(_) => {
                                            self.send_response(
                                                &mut writer,
                                                request.request_id,
                                                ServiceResponse::Error(protocol_error(
                                                    ServiceErrorCode::Internal,
                                                    "daemon dropped secret provisioning response".to_string(),
                                                    true,
                                                )),
                                            )
                                            .await?;
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                event_result = event_rx.recv(), if subscribed => {
                    match event_result {
                        Ok(event) => self.send_event(&mut writer, event).await?,
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            tracing::warn!("Client lagged behind, some events dropped");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }

        Ok(())
    }

    async fn send_response<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &mut BufWriter<W>,
        request_id: u64,
        response: ServiceResponse,
    ) -> Result<(), IpcError> {
        let json = serde_json::to_string(&ServerEnvelope::Response(ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response,
        }))?;
        writer.write_all(json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }

    async fn send_event<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &mut BufWriter<W>,
        event: ServiceUpdate,
    ) -> Result<(), IpcError> {
        let json = serde_json::to_string(&ServerEnvelope::Event(EventEnvelope {
            protocol_version: PROTOCOL_VERSION,
            event,
        }))?;
        writer.write_all(json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }
}

async fn try_connect(socket_path: &str) -> Result<Stream, IpcError> {
    if socket_path.starts_with('@') {
        let name = socket_path[1..]
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| IpcError::ConnectionFailed(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)))?;
        Stream::connect(name).await.map_err(IpcError::ConnectionFailed)
    } else {
        let name = socket_path
            .to_fs_name::<GenericFilePath>()
            .map_err(|e| IpcError::ConnectionFailed(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)))?;
        Stream::connect(name).await.map_err(IpcError::ConnectionFailed)
    }
}

fn protocol_error(code: ServiceErrorCode, message: String, retryable: bool) -> ServiceError {
    ServiceError {
        code,
        message,
        retryable,
        details: None,
    }
}
