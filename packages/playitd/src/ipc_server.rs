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
    EventEnvelope, IpcError, PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope, ServerEnvelope,
    ServiceRequest, ServiceResponse, get_default_socket_path, protocol_info,
};
use playit_ipc::model::{
    AccountLoginUrlResponse, AgentLifecycle, CommandResponse, ConnectionStats, SecretPathResponse,
    ServiceError, ServiceErrorCode, ServiceStatus, ServiceUpdate, SubscribeResponse,
    SubscriptionSnapshot,
};
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
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

#[cfg(target_os = "linux")]
const PLAYIT_SOCKET_GROUP_NAME: &str = "playit";
#[cfg(target_os = "linux")]
const PLAYIT_SOCKET_MODE: u32 = 0o660;

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
        self.configure_linux_socket_permissions()?;

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

    #[cfg(target_os = "linux")]
    fn configure_linux_socket_permissions(&self) -> Result<(), IpcError> {
        let Some(target) =
            linux_socket_permission_target(&self.socket_path, unsafe { libc::geteuid() as u32 })
        else {
            return Ok(());
        };

        if !Path::new(&target.path).exists() {
            return Err(IpcError::BindFailed(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("IPC socket {} was not created", target.path),
            )));
        }

        let Some(group_gid) = lookup_group_gid(target.group_name)? else {
            tracing::warn!(
                group = target.group_name,
                socket_path = %target.path,
                "IPC socket group is missing, leaving default socket permissions in place"
            );
            return Ok(());
        };

        apply_linux_socket_permissions(&target.path, group_gid, target.mode)?;
        Ok(())
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
                                    let lifecycle = self.state_cache.lifecycle().await;
                                    if !matches!(lifecycle, AgentLifecycle::WaitingForSecret) {
                                        self.send_response(
                                            &mut writer,
                                            request.request_id,
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
                                            request.request_id,
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
                                            request.request_id,
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
                                                request.request_id,
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

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinuxSocketPermissionTarget<'a> {
    path: &'a str,
    group_name: &'static str,
    mode: u32,
}

#[cfg(target_os = "linux")]
fn linux_socket_permission_target(
    socket_path: &str,
    effective_uid: u32,
) -> Option<LinuxSocketPermissionTarget<'_>> {
    if effective_uid != 0 || socket_path.starts_with('@') || socket_path.starts_with(r"\\.\pipe\") {
        return None;
    }

    Some(LinuxSocketPermissionTarget {
        path: socket_path,
        group_name: PLAYIT_SOCKET_GROUP_NAME,
        mode: PLAYIT_SOCKET_MODE,
    })
}

#[cfg(target_os = "linux")]
fn lookup_group_gid(group_name: &str) -> Result<Option<u32>, IpcError> {
    let group_name = CString::new(group_name).map_err(|e| {
        IpcError::BindFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid group name {group_name:?}: {e}"),
        ))
    })?;

    let mut group = std::mem::MaybeUninit::<libc::group>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buf_len = 1024usize;

    loop {
        let mut buf = vec![0u8; buf_len];
        let status = unsafe {
            libc::getgrnam_r(
                group_name.as_ptr(),
                group.as_mut_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut result,
            )
        };

        if status == 0 {
            if result.is_null() {
                return Ok(None);
            }

            let group = unsafe { group.assume_init() };
            return Ok(Some(group.gr_gid));
        }

        if status == libc::ERANGE {
            buf_len *= 2;
            continue;
        }

        return Err(IpcError::BindFailed(std::io::Error::from_raw_os_error(
            status,
        )));
    }
}

#[cfg(target_os = "linux")]
fn apply_linux_socket_permissions(
    socket_path: &str,
    group_gid: u32,
    mode: u32,
) -> Result<(), IpcError> {
    let path = Path::new(socket_path);
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|e| {
        IpcError::BindFailed(std::io::Error::new(
            e.kind(),
            format!("failed to chmod IPC socket {socket_path} to {mode:o}: {e}"),
        ))
    })?;

    let path_cstr = CString::new(path.as_os_str().as_bytes()).map_err(|e| {
        IpcError::BindFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid IPC socket path {socket_path:?}: {e}"),
        ))
    })?;

    let chown_status = unsafe { libc::chown(path_cstr.as_ptr(), u32::MAX, group_gid) };
    if chown_status != 0 {
        return Err(IpcError::BindFailed(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "failed to chown IPC socket {socket_path} to group gid {group_gid}: {}",
                std::io::Error::last_os_error()
            ),
        )));
    }

    Ok(())
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
    use super::secret_provisioning_state_error;
    use playit_ipc::model::{AgentLifecycle, ServiceError, ServiceErrorCode};

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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_socket_permissions_target_root_filesystem_socket() {
        let target = super::linux_socket_permission_target("/var/run/playitd.sock", 0)
            .expect("root filesystem socket should be configured");

        assert_eq!(target.group_name, super::PLAYIT_SOCKET_GROUP_NAME);
        assert_eq!(target.mode, super::PLAYIT_SOCKET_MODE);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_socket_permissions_skip_non_root() {
        assert!(super::linux_socket_permission_target("/var/run/playitd.sock", 1000).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_socket_permissions_skip_abstract_namespace() {
        assert!(super::linux_socket_permission_target("@playitd", 0).is_none());
    }
}
