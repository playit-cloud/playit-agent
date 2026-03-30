use std::path::{Path, PathBuf};
use std::sync::Arc;

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, ToFsName, ToNsName,
    tokio::{Listener, Stream, prelude::*},
};
use playit_agent_core::utils::now_milli;
use playit_api_client::PlayitApi;
use playit_ipc::ipc::{
    EventEnvelope, IpcError, PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope, ServerEnvelope,
    ServiceRequest, ServiceResponse, get_default_socket_path, protocol_info,
};
use playit_ipc::model::{
    AccountLoginUrlResponse, AgentLifecycle, CommandResponse, ConnectionStats,
    SecretPathResponse, ServiceError, ServiceErrorCode, ServiceStatus, ServiceUpdate,
    SubscribeResponse, SubscriptionSnapshot,
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
    secret_path: Option<PathBuf>,
    secret_provision_tx: Option<mpsc::Sender<SecretProvisionRequest>>,
    secret_provision_error: ServiceError,
    secret_reset_error: ServiceError,
    api: RwLock<Option<PlayitApi>>,
    guest_login_cache: RwLock<Option<(String, u64)>>,
}

impl IpcServer {
    pub async fn new_with_sender(
        socket_path: Option<String>,
        cancel_token: CancellationToken,
        event_tx: broadcast::Sender<ServiceUpdate>,
        secret_path: Option<PathBuf>,
        secret_provision_tx: Option<mpsc::Sender<SecretProvisionRequest>>,
        secret_provision_error: ServiceError,
        secret_reset_error: ServiceError,
    ) -> Result<Self, IpcError> {
        let socket_path = socket_path.unwrap_or_else(|| get_default_socket_path().to_string());

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
            secret_path,
            secret_provision_tx,
            secret_provision_error,
            secret_reset_error,
            api: RwLock::new(None),
            guest_login_cache: RwLock::new(None),
        })
    }

    pub fn event_sender(&self) -> broadcast::Sender<ServiceUpdate> {
        self.event_tx.clone()
    }

    pub fn state_cache(&self) -> Arc<StateCache> {
        self.state_cache.clone()
    }

    pub async fn set_api(&self, api: PlayitApi) {
        *self.api.write().await = Some(api);
    }

    pub async fn bind_listener(&self) -> Result<Listener, IpcError> {
        self.create_listener()
    }

    pub async fn run(self: Arc<Self>, listener: Listener) -> Result<(), IpcError> {
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

    fn create_listener(&self) -> Result<Listener, IpcError> {
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
                                            ServiceResponse::Error(self.secret_provision_error.clone()),
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
                                                    ServiceErrorCode::SecretWriteFailed,
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
                                ServiceRequest::ResetSecret => {
                                    match self.reset_secret().await {
                                        Ok(message) => {
                                            self.send_response(
                                                &mut writer,
                                                request.request_id,
                                                ServiceResponse::ResetSecret(CommandResponse {
                                                    accepted: true,
                                                    message: Some(message),
                                                }),
                                            )
                                            .await?;
                                        }
                                        Err(error) => {
                                            self.send_response(
                                                &mut writer,
                                                request.request_id,
                                                ServiceResponse::Error(error),
                                            )
                                            .await?;
                                        }
                                    }
                                }
                                ServiceRequest::GetSecretPath => {
                                    self.send_response(
                                        &mut writer,
                                        request.request_id,
                                        ServiceResponse::SecretPath(SecretPathResponse {
                                            secret_path: self
                                                .secret_path
                                                .as_ref()
                                                .map(|path| path.display().to_string()),
                                        }),
                                    )
                                    .await?;
                                }
                                ServiceRequest::GetAccountLoginUrl => {
                                    match self.get_account_login_url().await {
                                        Ok(login_url) => {
                                            self.send_response(
                                                &mut writer,
                                                request.request_id,
                                                ServiceResponse::AccountLoginUrl(
                                                    AccountLoginUrlResponse { login_url },
                                                ),
                                            )
                                            .await?;
                                        }
                                        Err(error) => {
                                            self.send_response(
                                                &mut writer,
                                                request.request_id,
                                                ServiceResponse::Error(error),
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

    async fn reset_secret(&self) -> Result<String, ServiceError> {
        let Some(secret_path) = &self.secret_path else {
            return Err(self.secret_reset_error.clone());
        };

        match tokio::fs::remove_file(secret_path).await {
            Ok(()) => Ok(format!(
                "Deleted secret file at {}. Restart playitd to reprovision a new secret.",
                secret_path.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(format!(
                "Secret file was already absent at {}.",
                secret_path.display()
            )),
            Err(error) => Err(protocol_error(
                ServiceErrorCode::SecretWriteFailed,
                format!(
                    "Failed to delete secret file {}: {error}",
                    secret_path.display()
                ),
                true,
            )),
        }
    }

    async fn get_account_login_url(&self) -> Result<String, ServiceError> {
        {
            let cache = self.guest_login_cache.read().await;
            if let Some((link, ts)) = &*cache {
                if now_milli().saturating_sub(*ts) < 15_000 {
                    return Ok(link.clone());
                }
            }
        }

        let api = self.api.read().await.clone().ok_or_else(|| {
            protocol_error(
                ServiceErrorCode::InvalidRequest,
                "playitd is not ready to generate a login URL yet".to_string(),
                true,
            )
        })?;

        let session = api.login_guest().await.map_err(|error| {
            protocol_error(
                ServiceErrorCode::Internal,
                format!("Failed to create login URL: {error:?}"),
                true,
            )
        })?;

        let link = format!(
            "https://playit.gg/login/guest-account/{}",
            session.session_key
        );
        *self.guest_login_cache.write().await = Some((link.clone(), now_milli()));
        Ok(link)
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
