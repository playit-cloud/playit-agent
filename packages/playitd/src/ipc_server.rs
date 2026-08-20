use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, ToFsName, ToNsName,
    tokio::{Listener, Stream, prelude::*},
};
#[cfg(target_os = "windows")]
use interprocess::os::windows::local_socket::ListenerOptionsExt;
use playit_ipc::endpoint::IpcEndpoint;
use playit_ipc::ipc::{
    EventEnvelope, HelloEnvelope, IPC_VERSION, IncomingRequestEnvelope, IpcError, IpcFrameWriter,
    ResponseEnvelope, ServerEnvelope, ServiceRequest, ServiceRequestOrUnknown, ServiceResponse,
    framed_parts, get_default_socket_path, is_known_request_type, protocol_info, try_connect,
};
use playit_ipc::model::{
    AccountLoginUrlResponse, AccountStatus, AgentLifecycle, AgentState, ClaimExchangeResponse,
    ClaimProgressResponse, ClaimSessionResponse, CommandResponse, ConnectionStats, NoticeState,
    PendingTunnelState, SecretPathResponse, ServiceError, ServiceErrorCode, ServicePhase,
    ServiceStatus, ServiceUpdate, SubscribeResponse, SubscriptionSnapshot, TunnelState,
};
use playit_model::{
    AppSnapshot, Command, Deadline, NoticePriority, OriginHost, OriginTarget, Phase, Problem,
    ProblemCode, ProblemReport, RetryPolicy, SecretInput, SecretNeed,
};
use playit_runtime::{
    ClaimProgress, CommandFailure, CommandOutput, SecretReset, SnapshotStore, SupervisorHandle,
};
use serde_json::json;
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

fn project_snapshot(snapshot: &AppSnapshot) -> SubscriptionSnapshot {
    SubscriptionSnapshot {
        status: project_status(snapshot),
        lifecycle: project_lifecycle(snapshot),
        stats: ConnectionStats {
            bytes_in: snapshot.traffic.bytes_in,
            bytes_out: snapshot.traffic.bytes_out,
            active_tcp: snapshot.traffic.active_tcp,
            active_udp: snapshot.traffic.active_udp,
        },
    }
}

fn project_status(snapshot: &AppSnapshot) -> ServiceStatus {
    let phase = match &snapshot.phase {
        Phase::Booting | Phase::Starting { .. } => ServicePhase::Starting,
        Phase::NeedsSecret {
            reason: SecretNeed::Invalid,
        } => ServicePhase::HasInvalidSecret,
        Phase::NeedsSecret { .. } => ServicePhase::WaitingForSecret,
        Phase::Blocked { problem, .. } if problem.code == ProblemCode::AgentDisabledOverLimit => {
            ServicePhase::DisabledOverLimit
        }
        Phase::Blocked { .. } => ServicePhase::Error,
        Phase::Online { .. } => ServicePhase::Running,
        Phase::Stopping | Phase::Stopped => ServicePhase::Stopping,
    };
    ServiceStatus {
        phase,
        pid: snapshot.service.process_id.unwrap_or_default(),
        uptime_secs: snapshot.service.uptime_secs,
        version: snapshot.service.version.clone().unwrap_or_default(),
        socket_path: snapshot.service.ipc_endpoint.clone().unwrap_or_default(),
        secret_path: snapshot.service.secret_location.clone(),
        has_secret: snapshot.service.has_secret,
        protocol: protocol_info(),
        last_error: project_snapshot_problem(snapshot),
    }
}

fn project_lifecycle(snapshot: &AppSnapshot) -> AgentLifecycle {
    match &snapshot.phase {
        Phase::Booting | Phase::Starting { .. } => AgentLifecycle::Starting,
        Phase::NeedsSecret {
            reason: SecretNeed::Invalid,
        } => AgentLifecycle::HasInvalidSecret(project_last_problem(snapshot)),
        Phase::NeedsSecret { .. } => AgentLifecycle::WaitingForSecret,
        Phase::Blocked { problem, .. } if problem.code == ProblemCode::AgentDisabledOverLimit => {
            AgentLifecycle::DisabledOverLimit(project_last_problem(snapshot))
        }
        Phase::Blocked { .. } => AgentLifecycle::Error(project_last_problem(snapshot)),
        Phase::Online { agent } => AgentLifecycle::Running(AgentState {
            version: snapshot.service.version.clone().unwrap_or_default(),
            tunnels: snapshot
                .catalog
                .accepted
                .tunnels()
                .iter()
                .map(|tunnel| TunnelState {
                    display_address: tunnel.public_address.clone(),
                    destination: project_origin_target(&tunnel.spec.target),
                    is_disabled: tunnel.disabled_reason.is_some(),
                    disabled_reason: tunnel.disabled_reason.clone(),
                })
                .collect(),
            pending_tunnels: snapshot
                .catalog
                .pending
                .iter()
                .map(|tunnel| PendingTunnelState {
                    id: tunnel.id.clone(),
                    status_msg: tunnel.status.clone(),
                })
                .collect(),
            notices: snapshot
                .notices
                .iter()
                .filter_map(|notice| notice.content.as_ref())
                .map(|notice| NoticeState {
                    priority: match notice.priority {
                        NoticePriority::Info => "info",
                        NoticePriority::Critical => "Critical",
                        NoticePriority::High => "High",
                        NoticePriority::Low => "Low",
                    }
                    .to_owned(),
                    message: notice.message.clone(),
                    resolve_link: notice.resolve_url.clone(),
                })
                .collect(),
            account_status: match snapshot.catalog.account_status {
                playit_model::AccountStatus::Unknown => AccountStatus::Unknown,
                playit_model::AccountStatus::Guest => AccountStatus::Guest,
                playit_model::AccountStatus::EmailNotVerified => AccountStatus::EmailNotVerified,
                playit_model::AccountStatus::Verified => AccountStatus::Verified,
            },
            agent_id: agent.as_str().to_owned(),
            login_link: snapshot.catalog.login_url.clone(),
            start_time: snapshot.service.started_at_millis,
        }),
        Phase::Stopping | Phase::Stopped => AgentLifecycle::Stopping,
    }
}

fn project_origin_target(target: &OriginTarget) -> String {
    let host = |host: &OriginHost| match host {
        OriginHost::Ip(address) => address.to_string(),
        OriginHost::Hostname(hostname) => hostname.as_str().to_owned(),
    };
    match target {
        OriginTarget::Port {
            host: origin,
            first_port,
        } => {
            format!("{}:{}", host(origin), first_port.get())
        }
        OriginTarget::Https {
            host: origin,
            http_port,
            https_port,
        } => format!(
            "{} (http: {}, https: {})",
            host(origin),
            http_port.get(),
            https_port.get()
        ),
    }
}

fn project_last_problem(snapshot: &AppSnapshot) -> ServiceError {
    project_snapshot_problem(snapshot)
        .unwrap_or_else(|| protocol_error(ServiceErrorCode::Internal, String::new(), false))
}

fn project_snapshot_problem(snapshot: &AppSnapshot) -> Option<ServiceError> {
    match &snapshot.phase {
        Phase::Blocked { problem, retry_at } => {
            let retry_at_millis = match retry_at {
                Deadline::AtMillis(value) => Some(*value),
                Deadline::Unscheduled => None,
            };
            Some(ServiceError {
                code: service_error_code(problem.code),
                message: snapshot
                    .last_problem
                    .as_ref()
                    .map(|report| report.detail.clone())
                    .unwrap_or_else(|| problem.code.as_str().to_owned()),
                retryable: problem.metadata.retry != RetryPolicy::Never,
                details: Some(problem_details(problem, retry_at_millis)),
            })
        }
        _ => snapshot.last_problem.as_ref().map(project_problem),
    }
}

fn project_problem(report: &ProblemReport) -> ServiceError {
    let code = service_error_code(report.problem.code);
    ServiceError {
        code,
        message: report.detail.clone(),
        retryable: report.problem.metadata.retry != RetryPolicy::Never,
        details: Some(problem_details(&report.problem, None)),
    }
}

fn service_error_code(code: ProblemCode) -> ServiceErrorCode {
    match code {
        ProblemCode::UnsupportedProtocol => ServiceErrorCode::UnsupportedProtocol,
        ProblemCode::InvalidRequest | ProblemCode::CommandNotAllowed => {
            ServiceErrorCode::InvalidRequest
        }
        ProblemCode::AgentDisabledOverLimit => ServiceErrorCode::AgentDisabledOverLimit,
        ProblemCode::InvalidSecret => ServiceErrorCode::InvalidSecret,
        ProblemCode::SecretPinned => ServiceErrorCode::SecretPinned,
        ProblemCode::ProvisioningUnavailable => ServiceErrorCode::ProvisioningUnavailable,
        ProblemCode::SecretWriteFailed => ServiceErrorCode::SecretWriteFailed,
        _ => ServiceErrorCode::Internal,
    }
}

fn problem_details(problem: &Problem, retry_at_millis: Option<u64>) -> serde_json::Value {
    json!({
        "problem_code": problem.code.as_str(),
        "severity": problem.metadata.severity.as_str(),
        "retry": problem.metadata.retry.as_str(),
        "action": problem.metadata.action.as_str(),
        "retry_at_millis": retry_at_millis,
    })
}

pub struct IpcServerConfig {
    pub socket_path: Option<String>,
    pub snapshots: Arc<SnapshotStore>,
    pub secret_path: Option<PathBuf>,
    pub commands: Option<SupervisorHandle>,
}

pub struct IpcServer {
    log_tx: broadcast::Sender<ServiceUpdate>,
    socket_path: String,
    cancel_token: CancellationToken,
    snapshots: Arc<SnapshotStore>,
    secret_path: Option<PathBuf>,
    commands: Option<SupervisorHandle>,
}

impl IpcServer {
    pub async fn new_with_sender(
        config: IpcServerConfig,
        cancel_token: CancellationToken,
        log_tx: broadcast::Sender<ServiceUpdate>,
    ) -> Result<Self, IpcError> {
        let socket_path = config
            .socket_path
            .unwrap_or_else(|| get_default_socket_path().to_string());
        let endpoint = IpcEndpoint::parse(socket_path.clone());

        if try_connect(&endpoint).await.is_ok() {
            return Err(IpcError::AlreadyRunning);
        }

        if !endpoint.is_windows_named_pipe() {
            if let Some(parent) = endpoint
                .filesystem_path()
                .and_then(Path::parent)
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Some(path) = endpoint.filesystem_path() {
                let _ = std::fs::remove_file(path);
            }
        }

        Ok(Self {
            log_tx,
            socket_path,
            cancel_token,
            snapshots: config.snapshots,
            secret_path: config.secret_path,
            commands: config.commands,
        })
    }

    pub async fn bind_listener(&self) -> Result<Listener, IpcError> {
        let listener = self.create_listener()?;

        #[cfg(target_os = "linux")]
        playit_platform::linux::configure_socket_permissions(&self.socket_path)
            .map_err(IpcError::BindFailed)?;

        Ok(listener)
    }

    pub async fn run(self: Arc<Self>, listener: Listener) -> Result<(), IpcError> {
        let mut clients = JoinSet::new();

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok(stream) => {
                            let server = self.clone();
                            clients.spawn(async move { server.handle_client(stream).await });
                        }
                        Err(e) => {
                            tracing::error!("Accept error: {e}");
                            // Avoid tight-loop logging if the listener enters a persistent failure state.
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
                joined = clients.join_next(), if !clients.is_empty() => {
                    match joined {
                        Some(Ok(Ok(()))) | None => {}
                        Some(Ok(Err(error))) if error.is_connection_closed() => {
                            tracing::debug!("Client disconnected: {error}");
                        }
                        Some(Ok(Err(error))) => {
                            tracing::warn!("Client connection error: {error}");
                        }
                        Some(Err(error)) => {
                            tracing::warn!("IPC client task failed: {error}");
                        }
                    }
                }
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("IPC server shutting down");
                    break;
                }
            }
        }

        clients.abort_all();
        while clients.join_next().await.is_some() {}

        Ok(())
    }

    fn create_listener(&self) -> Result<Listener, IpcError> {
        let endpoint = IpcEndpoint::parse(self.socket_path.clone());
        match endpoint {
            IpcEndpoint::Namespaced(name) => {
                let name = name
                    .clone()
                    .to_ns_name::<GenericNamespaced>()
                    .map_err(|e| {
                        IpcError::BindFailed(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            e,
                        ))
                    })?;
                let listener = ListenerOptions::new().name(name);
                #[cfg(target_os = "windows")]
                let listener = listener.security_descriptor(
                    playit_platform::windows::restricted_pipe_security_descriptor()
                        .map_err(IpcError::BindFailed)?,
                );
                listener.create_tokio().map_err(IpcError::BindFailed)
            }
            IpcEndpoint::Filesystem(path) => {
                let name = path.clone().to_fs_name::<GenericFilePath>().map_err(|e| {
                    IpcError::BindFailed(std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
                })?;
                let listener = ListenerOptions::new().name(name);
                #[cfg(target_os = "windows")]
                let listener = listener.security_descriptor(
                    playit_platform::windows::restricted_pipe_security_descriptor()
                        .map_err(IpcError::BindFailed)?,
                );
                listener.create_tokio().map_err(IpcError::BindFailed)
            }
        }
    }

    async fn handle_client(&self, stream: Stream) -> Result<(), IpcError> {
        let (reader, writer) = stream.split();
        let (mut reader, mut writer) = framed_parts(reader, writer);
        let mut log_rx = self.log_tx.subscribe();
        let mut snapshot_rx = self.snapshots.subscribe();
        let mut subscribed = false;
        let mut last_projection: Option<SubscriptionSnapshot> = None;

        self.send_hello(&mut writer).await?;

        loop {
            tokio::select! {
                read_result = reader.read_json::<IncomingRequestEnvelope>() => {
                    let envelope = read_result?;
                    let request_id = envelope.request_id;
                    let outcome = self.handle_request_envelope(envelope).await;

                    if outcome.subscribed {
                        subscribed = true;
                        last_projection = outcome.subscription.clone();
                    }

                    self.send_response(&mut writer, request_id, outcome.response).await?;
                }
                snapshot_result = snapshot_rx.changed(), if subscribed => {
                    match snapshot_result {
                        Ok(()) => {
                            let snapshot = snapshot_rx.borrow_and_update().clone();
                            let projection = project_snapshot(&snapshot);
                            self.send_snapshot_changes(&mut writer, last_projection.as_ref(), &projection).await?;
                            last_projection = Some(projection);
                        }
                        Err(_) => break,
                    }
                }
                log_result = log_rx.recv(), if subscribed => {
                    match log_result {
                        Ok(ServiceUpdate::Log(log)) => {
                            self.send_event(&mut writer, ServiceUpdate::Log(log)).await?
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            tracing::debug!("Client lagged behind, some log events were dropped");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_request_envelope(&self, envelope: IncomingRequestEnvelope) -> RequestOutcome {
        match self.validate_request_envelope(envelope) {
            Ok(request) => self.handle_service_request(request).await,
            Err(response) => RequestOutcome::respond(*response),
        }
    }

    fn validate_request_envelope(
        &self,
        envelope: IncomingRequestEnvelope,
    ) -> Result<ServiceRequest, Box<ServiceResponse>> {
        if envelope.ipc_version != IPC_VERSION {
            return Err(Box::new(ServiceResponse::Error(protocol_error(
                ServiceErrorCode::UnsupportedProtocol,
                format!(
                    "unsupported IPC version {} (expected {})",
                    envelope.ipc_version, IPC_VERSION
                ),
                false,
            ))));
        }

        match envelope.request {
            ServiceRequestOrUnknown::Known(request) => Ok(request),
            ServiceRequestOrUnknown::Unknown(unknown)
                if is_known_request_type(&unknown.type_name) =>
            {
                Err(Box::new(ServiceResponse::Error(protocol_error(
                    ServiceErrorCode::InvalidRequest,
                    format!("invalid IPC request payload for {}", unknown.type_name),
                    false,
                ))))
            }
            ServiceRequestOrUnknown::Unknown(unknown) => Err(Box::new(ServiceResponse::Error(
                invalid_request_type_error(&unknown.type_name),
            ))),
        }
    }

    async fn handle_service_request(&self, request: ServiceRequest) -> RequestOutcome {
        match request {
            ServiceRequest::Subscribe => {
                let snapshot = project_snapshot(&self.snapshots.snapshot());
                RequestOutcome {
                    response: self.subscribe_response(snapshot.clone()),
                    subscribed: true,
                    subscription: Some(snapshot),
                }
            }
            ServiceRequest::GetStatus => RequestOutcome::respond(self.status_response().await),
            ServiceRequest::GetState => RequestOutcome::respond(ServiceResponse::State(
                project_lifecycle(&self.snapshots.snapshot()),
            )),
            ServiceRequest::Stop => {
                tracing::info!("Stop request received, initiating shutdown");
                let response = match self.command(Command::Stop).await {
                    Ok(CommandOutput::Accepted) => ServiceResponse::Stop(CommandResponse {
                        accepted: true,
                        message: Some("shutdown requested".to_string()),
                    }),
                    Ok(_) => ServiceResponse::Error(provisioning_unavailable_error()),
                    Err(error) => ServiceResponse::Error(command_error(error)),
                };
                RequestOutcome::respond(response)
            }
            ServiceRequest::BeginClaim => {
                let response = match self.command(Command::BeginClaim).await {
                    Ok(CommandOutput::ClaimStarted(session)) => {
                        ServiceResponse::ClaimSession(ClaimSessionResponse {
                            claim_code: session.code,
                            claim_url: session.url,
                        })
                    }
                    Ok(_) => ServiceResponse::Error(provisioning_unavailable_error()),
                    Err(error) => ServiceResponse::Error(command_error(error)),
                };
                RequestOutcome::respond(response)
            }
            ServiceRequest::PollClaim { claim_code } => {
                let response = match self.command(Command::PollClaim { code: claim_code }).await {
                    Ok(CommandOutput::ClaimProgress(progress)) => {
                        ServiceResponse::ClaimProgress(match progress {
                            ClaimProgress::WaitingForVisit => {
                                ClaimProgressResponse::WaitingForVisit
                            }
                            ClaimProgress::WaitingForApproval => {
                                ClaimProgressResponse::WaitingForApproval
                            }
                            ClaimProgress::Approved => ClaimProgressResponse::Approved,
                            ClaimProgress::Rejected => ClaimProgressResponse::Rejected,
                        })
                    }
                    Ok(_) => ServiceResponse::Error(provisioning_unavailable_error()),
                    Err(error) => ServiceResponse::Error(command_error(error)),
                };
                RequestOutcome::respond(response)
            }
            ServiceRequest::ExchangeClaim { claim_code } => {
                let response = match self
                    .command(Command::ExchangeClaim { code: claim_code })
                    .await
                {
                    Ok(CommandOutput::ClaimPending(status)) => {
                        ServiceResponse::ClaimExchange(ClaimExchangeResponse::Pending(status))
                    }
                    Ok(CommandOutput::ClaimProvisioned) => {
                        ServiceResponse::ClaimExchange(ClaimExchangeResponse::Accepted)
                    }
                    Ok(_) => ServiceResponse::Error(provisioning_unavailable_error()),
                    Err(error) => ServiceResponse::Error(command_error(error)),
                };
                RequestOutcome::respond(response)
            }
            ServiceRequest::RefreshCatalog => {
                let response = match self.command(Command::RefreshCatalog).await {
                    Ok(CommandOutput::Accepted) => {
                        ServiceResponse::RefreshCatalog(CommandResponse {
                            accepted: true,
                            message: Some("catalog refresh requested".to_owned()),
                        })
                    }
                    Ok(_) => ServiceResponse::Error(provisioning_unavailable_error()),
                    Err(error) => ServiceResponse::Error(command_error(error)),
                };
                RequestOutcome::respond(response)
            }
            ServiceRequest::SetSecret { secret } => {
                RequestOutcome::respond(self.set_secret_response(secret).await)
            }
            ServiceRequest::ResetSecret => {
                RequestOutcome::respond(self.reset_secret_response().await)
            }
            ServiceRequest::GetSecretPath => {
                RequestOutcome::respond(ServiceResponse::SecretPath(SecretPathResponse {
                    secret_path: self
                        .secret_path
                        .as_ref()
                        .map(|path| path.display().to_string()),
                }))
            }
            ServiceRequest::GetAccountLoginUrl => {
                RequestOutcome::respond(self.account_login_url_response().await)
            }
        }
    }

    fn subscribe_response(&self, snapshot: SubscriptionSnapshot) -> ServiceResponse {
        ServiceResponse::Subscribe(SubscribeResponse {
            protocol: protocol_info(),
            snapshot,
        })
    }

    async fn status_response(&self) -> ServiceResponse {
        ServiceResponse::Status(project_status(&self.snapshots.snapshot()))
    }

    async fn set_secret_response(&self, secret: String) -> ServiceResponse {
        let secret = match SecretInput::new(secret) {
            Ok(secret) => secret,
            Err(_) => {
                return ServiceResponse::Error(command_error(
                    CommandFailure::new(ProblemCode::InvalidSecret).with_detail("secret is empty"),
                ));
            }
        };
        match self.command(Command::ProvisionSecret { secret }).await {
            Ok(CommandOutput::Accepted) => ServiceResponse::SetSecret(CommandResponse {
                accepted: true,
                message: Some("secret provisioned".to_string()),
            }),
            Ok(_) => ServiceResponse::Error(provisioning_unavailable_error()),
            Err(error) => ServiceResponse::Error(command_error(error)),
        }
    }

    async fn reset_secret_response(&self) -> ServiceResponse {
        match self.command(Command::ResetSecret).await {
            Ok(CommandOutput::SecretReset(reset)) => {
                tracing::info!("Secret reset, initiating shutdown");
                ServiceResponse::ResetSecret(CommandResponse {
                    accepted: true,
                    message: Some(reset_message(reset)),
                })
            }
            Ok(_) => ServiceResponse::Error(provisioning_unavailable_error()),
            Err(error) => ServiceResponse::Error(command_error(error)),
        }
    }

    async fn account_login_url_response(&self) -> ServiceResponse {
        match self.command(Command::CreateLoginUrl).await {
            Ok(CommandOutput::LoginUrl(login_url)) => {
                ServiceResponse::AccountLoginUrl(AccountLoginUrlResponse { login_url })
            }
            Ok(_) => ServiceResponse::Error(provisioning_unavailable_error()),
            Err(error) => ServiceResponse::Error(command_error(error)),
        }
    }

    async fn command(&self, command: Command) -> Result<CommandOutput, CommandFailure> {
        let Some(commands) = &self.commands else {
            return Err(CommandFailure::new(ProblemCode::EngineUnavailable));
        };
        commands.command(command).await
    }

    async fn send_response<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &mut IpcFrameWriter<W>,
        request_id: u64,
        response: ServiceResponse,
    ) -> Result<(), IpcError> {
        writer
            .write_json(&ServerEnvelope::Response(ResponseEnvelope {
                ipc_version: IPC_VERSION,
                request_id,
                response,
            }))
            .await
    }

    async fn send_hello<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &mut IpcFrameWriter<W>,
    ) -> Result<(), IpcError> {
        writer
            .write_json(&ServerEnvelope::Hello(HelloEnvelope {
                protocol: protocol_info(),
            }))
            .await
    }

    async fn send_event<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &mut IpcFrameWriter<W>,
        event: ServiceUpdate,
    ) -> Result<(), IpcError> {
        writer
            .write_json(&ServerEnvelope::Event(EventEnvelope {
                ipc_version: IPC_VERSION,
                event,
            }))
            .await
    }

    async fn send_snapshot_changes<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &mut IpcFrameWriter<W>,
        previous: Option<&SubscriptionSnapshot>,
        current: &SubscriptionSnapshot,
    ) -> Result<(), IpcError> {
        if previous.is_none_or(|value| !same_wire_value(&value.status, &current.status)) {
            self.send_event(writer, ServiceUpdate::Status(current.status.clone()))
                .await?;
        }
        if previous.is_none_or(|value| !same_wire_value(&value.lifecycle, &current.lifecycle)) {
            self.send_event(writer, ServiceUpdate::Lifecycle(current.lifecycle.clone()))
                .await?;
        }
        if previous.is_none_or(|value| !same_wire_value(&value.stats, &current.stats)) {
            self.send_event(writer, ServiceUpdate::Stats(current.stats.clone()))
                .await?;
        }
        Ok(())
    }
}

fn reset_message(reset: SecretReset) -> String {
    match reset {
        SecretReset::Deleted(path) => format!(
            "Deleted secret file at {}. Restart playitd to reprovision a new secret.",
            path.display()
        ),
        SecretReset::AlreadyAbsent(path) => {
            format!("Secret file was already absent at {}.", path.display())
        }
    }
}

fn command_error(error: CommandFailure) -> ServiceError {
    ServiceError {
        code: service_error_code(error.problem.code),
        message: error
            .detail
            .unwrap_or_else(|| error.problem.code.as_str().to_owned()),
        retryable: error.problem.metadata.retry != RetryPolicy::Never,
        details: Some(problem_details(&error.problem, error.retry_at_millis)),
    }
}

struct RequestOutcome {
    response: ServiceResponse,
    subscribed: bool,
    subscription: Option<SubscriptionSnapshot>,
}

impl RequestOutcome {
    fn respond(response: ServiceResponse) -> Self {
        Self {
            response,
            subscribed: false,
            subscription: None,
        }
    }
}

fn same_wire_value<T: serde::Serialize>(left: &T, right: &T) -> bool {
    serde_json::to_vec(left).expect("IPC model serializes")
        == serde_json::to_vec(right).expect("IPC model serializes")
}

fn protocol_error(code: ServiceErrorCode, message: String, retryable: bool) -> ServiceError {
    ServiceError {
        code,
        message,
        retryable,
        details: None,
    }
}

fn provisioning_unavailable_error() -> ServiceError {
    command_error(CommandFailure::new(ProblemCode::ProvisioningUnavailable))
}

fn invalid_request_type_error(request_type: &str) -> ServiceError {
    ServiceError {
        code: ServiceErrorCode::InvalidRequestType,
        message: format!("unknown IPC request type: {request_type}"),
        retryable: false,
        details: Some(json!({ "request_type": request_type })),
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU16, NonZeroU64};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{IpcServer, IpcServerConfig, SnapshotStore, project_snapshot, try_connect};
    use interprocess::local_socket::tokio::prelude::*;
    use playit_ipc::endpoint::IpcEndpoint;
    use playit_ipc::ipc::{
        IPC_VERSION, IpcClient, IpcError, RequestEnvelope, ResponseEnvelope, ServerEnvelope,
        ServiceRequest, ServiceResponse, protocol_info,
    };
    use playit_ipc::model::{AgentLifecycle, ServiceErrorCode, SubscribeResponse};
    use playit_model::{
        AgentIdentity, AppSnapshot, CatalogTunnel, Notice, NoticeContent, NoticePriority,
        OriginHost, OriginTarget, PendingTunnel, Phase, Problem, ProblemCode, ProxyProtocol,
        ServiceInfo, TrafficSnapshot, TunnelAvailability, TunnelCatalog, TunnelId, TunnelProtocol,
        TunnelSpec,
    };
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

    #[tokio::test]
    async fn snapshot_revision_increments_once_per_visible_publication() {
        let store = SnapshotStore::new(AppSnapshot::booting());
        let published = store.publish(|snapshot| {
            snapshot.phase = Phase::Starting { attempt: 1 };
            snapshot.traffic.bytes_in = 10;
            snapshot.service.has_secret = true;
        });
        assert_eq!(published.unwrap().revision, 1);

        let unchanged = store.publish(|snapshot| {
            snapshot.phase = Phase::Starting { attempt: 1 };
            snapshot.traffic.bytes_in = 10;
            snapshot.service.has_secret = true;
        });
        assert!(unchanged.is_none());
        assert_eq!(store.snapshot().revision, 1);
    }

    #[tokio::test]
    async fn slow_snapshot_subscriber_receives_latest_complete_revision() {
        let store = SnapshotStore::new(AppSnapshot::booting());
        let mut receiver = store.subscribe();

        for value in 1..=100 {
            store.publish(|snapshot| {
                snapshot.traffic.bytes_in = value;
                snapshot.traffic.bytes_out = value * 2;
            });
        }

        receiver.changed().await.unwrap();
        let snapshot = receiver.borrow_and_update().clone();
        assert_eq!(snapshot.revision, 100);
        assert_eq!(snapshot.traffic.bytes_in, 100);
        assert_eq!(snapshot.traffic.bytes_out, 200);
    }

    #[tokio::test]
    async fn concurrent_snapshot_publishers_do_not_lose_changes() {
        let store = Arc::new(SnapshotStore::new(AppSnapshot::booting()));
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..250 {
                    store.publish(|snapshot| snapshot.traffic.bytes_in += 1);
                    tokio::task::yield_now().await;
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        let snapshot = store.snapshot();
        assert_eq!(snapshot.revision, 1_000);
        assert_eq!(snapshot.traffic.bytes_in, 1_000);
    }

    #[test]
    fn ipc_v2_subscription_projection_matches_golden_json() {
        let mut snapshot = AppSnapshot::booting();
        snapshot.phase = Phase::Online {
            agent: AgentIdentity::new("agent-1"),
        };
        snapshot.service = ServiceInfo {
            process_id: Some(4242),
            uptime_secs: 17,
            started_at_millis: 1_700_000_000_000,
            version: Some("1.0.10".to_owned()),
            ipc_protocol: IPC_VERSION,
            ipc_endpoint: Some("/run/playit/playitd.sock".to_owned()),
            secret_location: Some("/etc/playit/playit.toml".to_owned()),
            has_secret: true,
        };
        snapshot.catalog.accepted = TunnelCatalog::try_new(
            1,
            1_700_000_000_000,
            vec![CatalogTunnel {
                spec: TunnelSpec {
                    id: TunnelId::new(NonZeroU64::MIN),
                    protocol: TunnelProtocol::Tcp,
                    target: OriginTarget::Port {
                        host: OriginHost::Ip("127.0.0.1".parse().unwrap()),
                        first_port: NonZeroU16::new(25565).unwrap(),
                    },
                    port_count: NonZeroU16::MIN,
                    proxy_protocol: ProxyProtocol::None,
                },
                availability: TunnelAvailability::Active,
                public_address: "demo.playit.gg:25565".to_owned(),
                disabled_reason: None,
            }],
        )
        .unwrap();
        snapshot.catalog.pending = vec![PendingTunnel {
            id: "pending-1".to_owned(),
            status: "allocating".to_owned(),
        }];
        snapshot.catalog.account_status = playit_model::AccountStatus::Verified;
        snapshot.catalog.login_url = Some("https://playit.gg/login/fixture".to_owned());
        snapshot.traffic = TrafficSnapshot {
            bytes_in: 10,
            bytes_out: 20,
            active_tcp: 1,
            active_udp: 2,
        };
        snapshot.notices = vec![Notice {
            problem: Problem::new(ProblemCode::RemoteNotice),
            content: Some(NoticeContent {
                priority: NoticePriority::Info,
                message: "fixture notice".to_owned(),
                resolve_url: Some("https://playit.gg/account".to_owned()),
            }),
        }];

        let envelope = ServerEnvelope::Response(ResponseEnvelope {
            ipc_version: IPC_VERSION,
            request_id: 1,
            response: ServiceResponse::Subscribe(SubscribeResponse {
                protocol: protocol_info(),
                snapshot: project_snapshot(&snapshot),
            }),
        });
        let expected = include_str!("../../playit-ipc/fixtures/ipc_v2_server_transcript.jsonl")
            .lines()
            .nth(1)
            .unwrap();
        assert_eq!(serde_json::to_string(&envelope).unwrap(), expected);
    }

    #[test]
    fn every_application_problem_projects_stable_action_and_retry_metadata() {
        for code in ProblemCode::ALL {
            let problem = Problem::new(code);
            let wire = super::command_error(playit_runtime::CommandFailure::new(code));
            let meaning = wire.meaning();
            assert_eq!(meaning.code, code.as_str());
            assert_eq!(meaning.action, problem.metadata.action.as_str());
            assert_eq!(meaning.retry, problem.metadata.retry.as_str());
        }
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
                IpcServerConfig {
                    socket_path: Some(socket_path.clone()),
                    snapshots: Arc::new(SnapshotStore::new(AppSnapshot::booting())),
                    secret_path: None,
                    commands: None,
                },
                cancel_token.clone(),
                event_tx,
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
        let endpoint = IpcEndpoint::parse(socket_path);
        let stream = try_connect(&endpoint).await.unwrap();
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
    async fn shutdown_closes_active_client_tasks() {
        let (_server, cancel_token, handle, socket_path) =
            spawn_test_server("shutdown-client").await;
        let (mut reader, _writer) = connect_raw(&socket_path).await;
        assert!(matches!(
            read_server_envelope(&mut reader).await,
            ServerEnvelope::Hello(_)
        ));

        shutdown_server(cancel_token, handle).await;

        let mut line = String::new();
        let bytes_read = tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line))
            .await
            .expect("server shutdown closes the client promptly")
            .unwrap();
        assert_eq!(bytes_read, 0);
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
