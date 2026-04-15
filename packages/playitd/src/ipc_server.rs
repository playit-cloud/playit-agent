use std::path::{Path, PathBuf};
use std::sync::Arc;

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, ToFsName, ToNsName,
    tokio::{Listener, Stream, prelude::*},
};
#[cfg(target_os = "windows")]
use interprocess::os::windows::{
    local_socket::ListenerOptionsExt,
    security_descriptor::{AsSecurityDescriptorMutExt, SecurityDescriptor},
};
use playit_agent_core::utils::now_milli;
use playit_api_client::PlayitApi;
use playit_ipc::ipc::{
    EventEnvelope, HelloEnvelope, IPC_VERSION, IncomingRequestEnvelope, IpcError, ResponseEnvelope,
    ServerEnvelope, ServiceRequest, ServiceRequestOrUnknown, ServiceResponse,
    get_default_socket_path, is_known_request_type, protocol_info,
};
use playit_ipc::model::{
    AccountLoginUrlResponse, AgentLifecycle, CommandResponse, ConnectionStats, SecretPathResponse,
    ServiceError, ServiceErrorCode, ServiceStatus, ServiceUpdate, SubscribeResponse,
    SubscriptionSnapshot,
};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const ACCOUNT_AGENTS_URL: &str = "https://playit.gg/account/agents";
const ACCOUNT_UPGRADE_URL: &str = "https://playit.gg/account/upgrade";

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
        let listener = self.create_listener()?;

        #[cfg(target_os = "linux")]
        crate::linux::configure_socket_permissions(&self.socket_path)?;

        Ok(listener)
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
                .map_err(|e| {
                    IpcError::BindFailed(std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
                })?;
            let listener = ListenerOptions::new().name(name);
            #[cfg(target_os = "windows")]
            let listener = listener.security_descriptor(world_access_security_descriptor()?);
            listener.create_tokio().map_err(IpcError::BindFailed)
        } else {
            let name = self
                .socket_path
                .clone()
                .to_fs_name::<GenericFilePath>()
                .map_err(|e| {
                    IpcError::BindFailed(std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
                })?;
            let listener = ListenerOptions::new().name(name);
            #[cfg(target_os = "windows")]
            let listener = listener.security_descriptor(world_access_security_descriptor()?);
            listener.create_tokio().map_err(IpcError::BindFailed)
        }
    }

    async fn handle_client(&self, stream: Stream) -> Result<(), IpcError> {
        let (reader, writer) = stream.split();
        let mut reader = BufReader::new(reader);
        let mut writer = BufWriter::new(writer);
        let mut line = String::new();
        let mut event_rx = self.event_tx.subscribe();
        let mut subscribed = false;

        self.send_hello(&mut writer).await?;

        loop {
            tokio::select! {
                read_result = reader.read_line(&mut line) => {
                    match read_result {
                        Ok(0) => break,
                        Ok(_) => {
                            let request =
                                serde_json::from_str::<IncomingRequestEnvelope>(line.trim())?;
                            line.clear();
                            let request_id = request.request_id;

                            if request.ipc_version != IPC_VERSION {
                                self.send_response(
                                    &mut writer,
                                    request_id,
                                    ServiceResponse::Error(protocol_error(
                                        ServiceErrorCode::UnsupportedProtocol,
                                        format!(
                                            "unsupported IPC version {} (expected {})",
                                            request.ipc_version,
                                            IPC_VERSION
                                        ),
                                        false,
                                    )),
                                ).await?;
                                continue;
                            }

                            let request = match request.request {
                                ServiceRequestOrUnknown::Known(request) => request,
                                ServiceRequestOrUnknown::Unknown(unknown) => {
                                    if is_known_request_type(&unknown.type_name) {
                                        let message = format!(
                                            "invalid IPC request payload for {}",
                                            unknown.type_name
                                        );
                                        self.send_response(
                                            &mut writer,
                                            request_id,
                                            ServiceResponse::Error(protocol_error(
                                                ServiceErrorCode::InvalidRequest,
                                                message,
                                                false,
                                            )),
                                        )
                                        .await?;
                                        continue;
                                    }
                                    self.send_response(
                                        &mut writer,
                                        request_id,
                                        ServiceResponse::Error(invalid_request_type_error(
                                            &unknown.type_name,
                                        )),
                                    )
                                    .await?;
                                    continue;
                                }
                            };

                            match request {
                                ServiceRequest::Subscribe => {
                                    subscribed = true;
                                    let snapshot = self.state_cache.subscription_snapshot().await;
                                    self.send_response(
                                        &mut writer,
                                        request_id,
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
                                        request_id,
                                        ServiceResponse::Status(status),
                                    )
                                    .await?;
                                }
                                ServiceRequest::GetState => {
                                    self.send_response(
                                        &mut writer,
                                        request_id,
                                        ServiceResponse::State(self.state_cache.lifecycle().await),
                                    )
                                    .await?;
                                }
                                ServiceRequest::Stop => {
                                    tracing::info!("Stop request received, initiating shutdown");
                                    self.cancel_token.cancel();
                                    self.send_response(
                                        &mut writer,
                                        request_id,
                                        ServiceResponse::Stop(CommandResponse {
                                            accepted: true,
                                            message: Some("shutdown requested".to_string()),
                                        }),
                                    )
                                    .await?;
                                }
                                ServiceRequest::SetSecret { secret } => {
                                    let lifecycle = self.state_cache.lifecycle().await;
                                    if !matches!(lifecycle, AgentLifecycle::WaitingForSecret) {
                                        self.send_response(
                                            &mut writer,
                                            request_id,
                                            ServiceResponse::Error(
                                                secret_provisioning_state_error(&lifecycle),
                                            ),
                                        )
                                        .await?;
                                        continue;
                                    }

                                    let Some(secret_provision_tx) = &self.secret_provision_tx else {
                                        self.send_response(
                                            &mut writer,
                                            request_id,
                                            ServiceResponse::Error(self.secret_provision_error.clone()),
                                        )
                                        .await?;
                                        continue;
                                    };

                                    let (response_tx, response_rx) = oneshot::channel();
                                    if secret_provision_tx
                                        .send(SecretProvisionRequest { secret, response_tx })
                                        .await
                                        .is_err()
                                    {
                                        self.send_response(
                                            &mut writer,
                                            request_id,
                                            ServiceResponse::Error(protocol_error(
                                                ServiceErrorCode::ProvisioningUnavailable,
                                                "playitd is no longer waiting for secret provisioning"
                                                    .to_string(),
                                                true,
                                            )),
                                        )
                                        .await?;
                                        continue;
                                    }

                                    match response_rx.await {
                                        Ok(Ok(())) => {
                                            self.send_response(
                                                &mut writer,
                                                request_id,
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
                                                request_id,
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
                                                request_id,
                                                ServiceResponse::Error(protocol_error(
                                                    ServiceErrorCode::ProvisioningUnavailable,
                                                    "playitd is no longer waiting for secret provisioning"
                                                        .to_string(),
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
                                                request_id,
                                                ServiceResponse::ResetSecret(CommandResponse {
                                                    accepted: true,
                                                    message: Some(message),
                                                }),
                                            )
                                            .await?;

                                            tracing::info!("Secret reset, initiating shutdown");
                                            self.cancel_token.cancel();
                                        }
                                        Err(error) => {
                                            self.send_response(
                                                &mut writer,
                                                request_id,
                                                ServiceResponse::Error(error),
                                            )
                                            .await?;
                                        }
                                    }
                                }
                                ServiceRequest::GetSecretPath => {
                                    self.send_response(
                                        &mut writer,
                                        request_id,
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
                                                request_id,
                                                ServiceResponse::AccountLoginUrl(
                                                    AccountLoginUrlResponse { login_url },
                                                ),
                                            )
                                            .await?;
                                        }
                                        Err(error) => {
                                            self.send_response(
                                                &mut writer,
                                                request_id,
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
            ipc_version: IPC_VERSION,
            request_id,
            response,
        }))?;
        writer.write_all(json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }

    async fn send_hello<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &mut BufWriter<W>,
    ) -> Result<(), IpcError> {
        let json = serde_json::to_string(&ServerEnvelope::Hello(HelloEnvelope {
            protocol: protocol_info(),
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
            ipc_version: IPC_VERSION,
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

#[cfg(target_os = "windows")]
fn world_access_security_descriptor() -> Result<SecurityDescriptor, IpcError> {
    let mut descriptor = SecurityDescriptor::new().map_err(IpcError::BindFailed)?;
    unsafe {
        // Allow non-elevated user sessions to connect to the service-owned named pipe.
        descriptor
            .set_dacl(std::ptr::null_mut(), false)
            .map_err(IpcError::BindFailed)?;
    }
    Ok(descriptor)
}

async fn try_connect(socket_path: &str) -> Result<Stream, IpcError> {
    if socket_path.starts_with('@') {
        let name = socket_path[1..]
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| {
                IpcError::ConnectionFailed(std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
            })?;
        Stream::connect(name)
            .await
            .map_err(IpcError::ConnectionFailed)
    } else {
        let name = socket_path.to_fs_name::<GenericFilePath>().map_err(|e| {
            IpcError::ConnectionFailed(std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
        })?;
        Stream::connect(name)
            .await
            .map_err(IpcError::ConnectionFailed)
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

fn invalid_request_type_error(request_type: &str) -> ServiceError {
    ServiceError {
        code: ServiceErrorCode::InvalidRequestType,
        message: format!("unknown IPC request type: {request_type}"),
        retryable: false,
        details: Some(json!({ "request_type": request_type })),
    }
}

fn over_limit_guidance() -> String {
    format!(
        "Visit {ACCOUNT_AGENTS_URL} to delete unused agents\nVisit {ACCOUNT_UPGRADE_URL} to increase your agent limit"
    )
}

fn secret_provisioning_state_error(lifecycle: &AgentLifecycle) -> ServiceError {
    match lifecycle {
        AgentLifecycle::WaitingForSecret => protocol_error(
            ServiceErrorCode::ProvisioningUnavailable,
            "playitd is not ready for secret provisioning".to_string(),
            true,
        ),
        AgentLifecycle::HasInvalidSecret(error) => protocol_error(
            ServiceErrorCode::ProvisioningUnavailable,
            format!(
                "playitd is not waiting for a new secret because its current secret is invalid: {}",
                error.message
            ),
            false,
        ),
        AgentLifecycle::DisabledOverLimit(error) => protocol_error(
            ServiceErrorCode::ProvisioningUnavailable,
            format!(
                "Setup is unavailable because this account is over the agent limit.\n{}\nReason: {}",
                over_limit_guidance(),
                error.message
            ),
            false,
        ),
        AgentLifecycle::Starting => protocol_error(
            ServiceErrorCode::ProvisioningUnavailable,
            "playitd is starting and not waiting for secret provisioning".to_string(),
            true,
        ),
        AgentLifecycle::Running(_) => protocol_error(
            ServiceErrorCode::ProvisioningUnavailable,
            "playitd already has a configured secret and is not waiting for provisioning"
                .to_string(),
            false,
        ),
        AgentLifecycle::Stopping => protocol_error(
            ServiceErrorCode::ProvisioningUnavailable,
            "playitd is stopping and cannot accept secret provisioning".to_string(),
            true,
        ),
        AgentLifecycle::Error(error) => protocol_error(
            ServiceErrorCode::ProvisioningUnavailable,
            format!(
                "playitd reported an error and is not waiting for secret provisioning: {}",
                error.message
            ),
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{IpcServer, protocol_error, secret_provisioning_state_error, try_connect};
    use interprocess::local_socket::tokio::prelude::*;
    use playit_ipc::ipc::{
        IPC_VERSION, IpcClient, IpcError, RequestEnvelope, ServerEnvelope, ServiceRequest,
        ServiceResponse,
    };
    use playit_ipc::model::{AgentLifecycle, ServiceError, ServiceErrorCode};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;

    fn test_socket_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!(
                "playitd-ipc-{name}-{}-{}.sock",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .display()
            .to_string()
    }

    async fn spawn_test_server(
        name: &str,
    ) -> (
        Arc<IpcServer>,
        CancellationToken,
        tokio::task::JoinHandle<Result<(), IpcError>>,
        String,
    ) {
        let socket_path = test_socket_path(name);
        let cancel_token = CancellationToken::new();
        let (event_tx, _) = broadcast::channel(8);
        let server = Arc::new(
            IpcServer::new_with_sender(
                Some(socket_path.clone()),
                cancel_token.clone(),
                event_tx,
                None,
                None,
                protocol_error(
                    ServiceErrorCode::ProvisioningUnavailable,
                    "provisioning unavailable".to_string(),
                    false,
                ),
                protocol_error(
                    ServiceErrorCode::SecretWriteFailed,
                    "secret reset unavailable".to_string(),
                    false,
                ),
            )
            .await
            .unwrap(),
        );
        let listener = server.bind_listener().await.unwrap();
        let handle = tokio::spawn(server.clone().run(listener));

        (server, cancel_token, handle, socket_path)
    }

    async fn shutdown_server(
        cancel_token: CancellationToken,
        handle: tokio::task::JoinHandle<Result<(), IpcError>>,
    ) {
        cancel_token.cancel();
        let _ = handle.await.unwrap();
    }

    async fn connect_raw(
        socket_path: &str,
    ) -> (
        BufReader<interprocess::local_socket::tokio::RecvHalf>,
        BufWriter<interprocess::local_socket::tokio::SendHalf>,
    ) {
        let stream = try_connect(socket_path).await.unwrap();
        let (reader, writer) = stream.split();
        (BufReader::new(reader), BufWriter::new(writer))
    }

    async fn read_server_envelope<R: tokio::io::AsyncBufRead + Unpin>(
        reader: &mut R,
    ) -> ServerEnvelope {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    #[test]
    fn provisioning_rejects_running_daemon() {
        let error = secret_provisioning_state_error(&AgentLifecycle::Running(Default::default()));
        assert!(matches!(
            error.code,
            ServiceErrorCode::ProvisioningUnavailable
        ));
        assert!(!error.retryable);
        assert!(error.message.contains("already has a configured secret"));
    }

    #[test]
    fn provisioning_rejects_invalid_secret_state() {
        let error =
            secret_provisioning_state_error(&AgentLifecycle::HasInvalidSecret(ServiceError {
                code: ServiceErrorCode::InvalidSecret,
                message: "bad secret".to_string(),
                retryable: true,
                details: None,
            }));
        assert!(matches!(
            error.code,
            ServiceErrorCode::ProvisioningUnavailable
        ));
        assert!(!error.retryable);
        assert!(error.message.contains("bad secret"));
    }

    #[tokio::test]
    async fn server_writes_hello_immediately() {
        let (_server, cancel_token, handle, socket_path) = spawn_test_server("hello").await;
        let mut client = IpcClient::connect_with_path(&socket_path).await.unwrap();
        assert_eq!(client.server_protocol().ipc_version, IPC_VERSION);
        assert!(!client.server_protocol().capabilities.is_empty());
        let lifecycle = client.lifecycle().await.unwrap();
        assert!(matches!(lifecycle, AgentLifecycle::Starting));
        shutdown_server(cancel_token, handle).await;
    }

    #[tokio::test]
    async fn unknown_request_type_returns_error_and_connection_stays_open() {
        let (_server, cancel_token, handle, socket_path) = spawn_test_server("unknown-type").await;
        let (mut reader, mut writer) = connect_raw(&socket_path).await;

        let hello = read_server_envelope(&mut reader).await;
        assert!(matches!(hello, ServerEnvelope::Hello(_)));

        let unknown_request = serde_json::json!({
            "ipc_version": IPC_VERSION,
            "request_id": 1,
            "request": {
                "type": "future_request",
                "data": {"flag": true}
            }
        });
        writer
            .write_all(serde_json::to_string(&unknown_request).unwrap().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();

        let response = read_server_envelope(&mut reader).await;
        match response {
            ServerEnvelope::Response(response) => match response.response {
                ServiceResponse::Error(error) => {
                    assert!(matches!(error.code, ServiceErrorCode::InvalidRequestType));
                    assert_eq!(
                        error.details.unwrap()["request_type"],
                        serde_json::Value::String("future_request".to_string())
                    );
                }
                other => panic!("expected error response, got {other:?}"),
            },
            other => panic!("expected response frame, got {other:?}"),
        }

        let valid_request = serde_json::to_string(&RequestEnvelope {
            ipc_version: IPC_VERSION,
            request_id: 2,
            request: ServiceRequest::GetState,
        })
        .unwrap();
        writer.write_all(valid_request.as_bytes()).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();

        let response = read_server_envelope(&mut reader).await;
        match response {
            ServerEnvelope::Response(response) => {
                assert_eq!(response.request_id, 2);
                assert!(matches!(
                    response.response,
                    ServiceResponse::State(AgentLifecycle::Starting)
                ));
            }
            other => panic!("expected response frame, got {other:?}"),
        }

        shutdown_server(cancel_token, handle).await;
    }

    #[tokio::test]
    async fn invalid_payload_for_known_request_returns_invalid_request() {
        let (_server, cancel_token, handle, socket_path) =
            spawn_test_server("invalid-payload").await;
        let (mut reader, mut writer) = connect_raw(&socket_path).await;

        let _ = read_server_envelope(&mut reader).await;

        let invalid_request = serde_json::json!({
            "ipc_version": IPC_VERSION,
            "request_id": 1,
            "request": {
                "type": "set_secret"
            }
        });
        writer
            .write_all(serde_json::to_string(&invalid_request).unwrap().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();

        let response = read_server_envelope(&mut reader).await;
        match response {
            ServerEnvelope::Response(response) => match response.response {
                ServiceResponse::Error(error) => {
                    assert!(matches!(error.code, ServiceErrorCode::InvalidRequest));
                    assert!(error.message.contains("set_secret"));
                }
                other => panic!("expected error response, got {other:?}"),
            },
            other => panic!("expected response frame, got {other:?}"),
        }

        shutdown_server(cancel_token, handle).await;
    }

    #[tokio::test]
    async fn mismatched_ipc_version_returns_unsupported_protocol() {
        let (_server, cancel_token, handle, socket_path) =
            spawn_test_server("version-mismatch").await;
        let (mut reader, mut writer) = connect_raw(&socket_path).await;

        let _ = read_server_envelope(&mut reader).await;

        let request = serde_json::to_string(&RequestEnvelope {
            ipc_version: IPC_VERSION + 1,
            request_id: 1,
            request: ServiceRequest::GetState,
        })
        .unwrap();
        writer.write_all(request.as_bytes()).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();

        let response = read_server_envelope(&mut reader).await;
        match response {
            ServerEnvelope::Response(response) => match response.response {
                ServiceResponse::Error(error) => {
                    assert!(matches!(error.code, ServiceErrorCode::UnsupportedProtocol));
                }
                other => panic!("expected error response, got {other:?}"),
            },
            other => panic!("expected response frame, got {other:?}"),
        }

        shutdown_server(cancel_token, handle).await;
    }
}
