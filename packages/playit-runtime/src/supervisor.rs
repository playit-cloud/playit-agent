use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use playit_agent_core::gateway::{
    GatewayErrorCode, GatewayOrigin, GatewayRunData, PlayitGateway, RunDataContext,
};
use playit_agent_core::playit_agent::{EngineExit, ServiceExit};
use playit_model::{
    AgentIdentity, AppSnapshot, AppState, Command, CommandDecision, Deadline, Effect, Event,
    EventDecision, Phase, Problem, ProblemCode, ProblemReport, SecretInput, ServiceInfo,
    TrafficSnapshot, decide_command, reduce,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    ClaimExchange, ClaimFailure, ClaimMode, ClaimProgress, ClaimService, ClaimSession,
    GeneratedClientGateway,
};

pub type BoxSleep = Pin<Box<dyn Future<Output = ()> + Send>>;

pub trait Clock: Send + Sync {
    fn now_millis(&self) -> u64;
    fn sleep_until(&self, deadline_millis: u64) -> BoxSleep;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn sleep_until(&self, deadline_millis: u64) -> BoxSleep {
        let delay = Duration::from_millis(deadline_millis.saturating_sub(self.now_millis()));
        Box::pin(tokio::time::sleep(delay))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorPolicy {
    pub refresh_interval: Duration,
    pub refresh_retry: Duration,
    pub agent_limit_retry: Duration,
    pub stats_interval: Duration,
    pub shutdown_deadline: Duration,
    pub gateway_timeout: Duration,
}

impl Default for SupervisorPolicy {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(3),
            refresh_retry: Duration::from_secs(3),
            agent_limit_retry: Duration::from_secs(30),
            stats_interval: Duration::from_millis(100),
            shutdown_deadline: Duration::from_secs(5),
            gateway_timeout: Duration::from_secs(10),
        }
    }
}

impl SupervisorPolicy {
    pub fn deadline_after(&self, now_millis: u64, delay: Duration) -> u64 {
        now_millis.saturating_add(delay.as_millis() as u64)
    }

    pub fn retry_deadline(&self, now_millis: u64, code: GatewayErrorCode) -> u64 {
        let delay = if code == GatewayErrorCode::AgentDisabledOverLimit {
            self.agent_limit_retry
        } else {
            self.refresh_retry
        };
        self.deadline_after(now_millis, delay)
    }
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub api_base: String,
    pub version: String,
    pub start_time: u64,
    pub service: ServiceInfo,
    pub policy: SupervisorPolicy,
}

#[derive(Clone, PartialEq, Eq)]
pub enum SecretState {
    Ready(String),
    Missing,
    Invalid(String),
}

impl std::fmt::Debug for SecretState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(_) => formatter.write_str("Ready([REDACTED])"),
            Self::Missing => formatter.write_str("Missing"),
            Self::Invalid(detail) => formatter.debug_tuple("Invalid").field(detail).finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretStoreError {
    Pinned,
    Io(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretReset {
    Deleted(PathBuf),
    AlreadyAbsent(PathBuf),
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn load(&self) -> SecretState;
    async fn persist(&self, secret: &SecretInput) -> Result<(), SecretStoreError>;
    async fn reset(&self) -> Result<SecretReset, SecretStoreError>;
    fn path(&self) -> Option<&Path>;
    fn can_provision(&self) -> bool;
}

pub struct InlineSecretStore {
    secret: String,
}

impl InlineSecretStore {
    pub fn new(secret: String) -> Self {
        Self { secret }
    }
}

#[async_trait]
impl SecretStore for InlineSecretStore {
    async fn load(&self) -> SecretState {
        match validate_secret(self.secret.trim()) {
            Ok(secret) => SecretState::Ready(secret),
            Err(error) => {
                SecretState::Invalid(format!("Invalid secret passed via --secret: {error}"))
            }
        }
    }

    async fn persist(&self, _secret: &SecretInput) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Pinned)
    }

    async fn reset(&self) -> Result<SecretReset, SecretStoreError> {
        Err(SecretStoreError::Pinned)
    }

    fn path(&self) -> Option<&Path> {
        None
    }

    fn can_provision(&self) -> bool {
        false
    }
}

pub struct FileSecretStore {
    path: PathBuf,
}

impl FileSecretStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn load(&self) -> SecretState {
        let content = match tokio::fs::read_to_string(&self.path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return SecretState::Missing;
            }
            Err(error) => {
                return SecretState::Invalid(format!(
                    "Failed to read secret file {}: {error}",
                    self.path.display()
                ));
            }
        };
        match parse_secret_file(&content) {
            Ok(secret) => SecretState::Ready(secret),
            Err(()) => {
                SecretState::Invalid(format!("invalid secret file at {}", self.path.display()))
            }
        }
    }

    async fn persist(&self, secret: &SecretInput) -> Result<(), SecretStoreError> {
        persist_secret_file(&self.path, secret.expose())
            .await
            .map_err(SecretStoreError::Io)
    }

    async fn reset(&self) -> Result<SecretReset, SecretStoreError> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(SecretReset::Deleted(self.path.clone())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(SecretReset::AlreadyAbsent(self.path.clone()))
            }
            Err(error) => Err(SecretStoreError::Io(format!(
                "Failed to delete secret file {}: {error}",
                self.path.display()
            ))),
        }
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn can_provision(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStartError {
    InvalidSecret(String),
    AgentDisabledOverLimit(String),
    Failed(String),
}

#[async_trait]
pub trait OriginPublisher: Send + Sync {
    async fn replace(&self, origins: &[GatewayOrigin]);
}

pub trait TrafficSource: Send + Sync {
    fn snapshot(&self) -> TrafficSnapshot;
}

pub struct EngineChild {
    cancel: CancellationToken,
    exit: JoinHandle<EngineExit>,
    origins: Arc<dyn OriginPublisher>,
    traffic: Arc<dyn TrafficSource>,
}

impl EngineChild {
    pub fn new(
        cancel: CancellationToken,
        exit: JoinHandle<EngineExit>,
        origins: Arc<dyn OriginPublisher>,
        traffic: Arc<dyn TrafficSource>,
    ) -> Self {
        Self {
            cancel,
            exit,
            origins,
            traffic,
        }
    }
}

#[async_trait]
pub trait EnginePort: Send + Sync {
    async fn start(
        &self,
        gateway: Arc<dyn PlayitGateway>,
        origins: &[GatewayOrigin],
    ) -> Result<EngineChild, EngineStartError>;
}

pub struct ServiceChild {
    cancel: CancellationToken,
    exit: JoinHandle<ServiceExit>,
}

impl ServiceChild {
    pub fn new(cancel: CancellationToken, exit: JoinHandle<ServiceExit>) -> Self {
        Self { cancel, exit }
    }
}

pub trait IpcPort: Send {
    fn start(self: Box<Self>) -> ServiceChild;
}

pub trait GatewayFactory: Send + Sync {
    fn create(&self, api_base: &str, secret: &str) -> Arc<dyn PlayitGateway>;
}

#[derive(Debug, Default)]
pub struct GeneratedGatewayFactory;

impl GatewayFactory for GeneratedGatewayFactory {
    fn create(&self, api_base: &str, secret: &str) -> Arc<dyn PlayitGateway> {
        Arc::new(GeneratedClientGateway::new(
            api_base.to_owned(),
            secret.to_owned(),
        ))
    }
}

pub struct SnapshotStore {
    sender: watch::Sender<Arc<AppSnapshot>>,
}

impl SnapshotStore {
    pub fn new(snapshot: AppSnapshot) -> Self {
        let (sender, _) = watch::channel(Arc::new(snapshot));
        Self { sender }
    }

    pub fn snapshot(&self) -> Arc<AppSnapshot> {
        self.sender.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<AppSnapshot>> {
        self.sender.subscribe()
    }

    pub fn publish(&self, update: impl FnOnce(&mut AppSnapshot)) -> Option<Arc<AppSnapshot>> {
        let mut update = Some(update);
        let mut published = None;
        self.sender.send_if_modified(|current| {
            let mut next = (**current).clone();
            update.take().expect("snapshot update runs once")(&mut next);
            next.revision = current.revision;
            if next == **current {
                return false;
            }
            next.revision = current
                .revision
                .checked_add(1)
                .expect("snapshot revision exhausted");
            let next = Arc::new(next);
            published = Some(next.clone());
            *current = next;
            true
        });
        published
    }

    fn apply_state(&self, state: AppState) {
        self.publish(|snapshot| {
            let revision = snapshot.revision;
            *snapshot = state.snapshot;
            snapshot.revision = revision;
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutput {
    Accepted,
    ClaimStarted(ClaimSession),
    ClaimProgress(ClaimProgress),
    ClaimPending(String),
    ClaimProvisioned,
    LoginUrl(String),
    SecretReset(SecretReset),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFailure {
    pub problem: Problem,
    pub detail: Option<String>,
    pub retry_at_millis: Option<u64>,
}

impl CommandFailure {
    pub const fn new(code: ProblemCode) -> Self {
        Self {
            problem: Problem::new(code),
            detail: None,
            retry_at_millis: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub const fn with_retry_at(mut self, retry_at_millis: u64) -> Self {
        self.retry_at_millis = Some(retry_at_millis);
        self
    }
}

pub type CommandError = CommandFailure;

struct CommandRequest {
    command: Command,
    response: oneshot::Sender<Result<CommandOutput, CommandError>>,
}

#[derive(Clone)]
pub struct SupervisorHandle {
    tx: mpsc::Sender<CommandRequest>,
    effect_cancel: CancellationToken,
}

impl SupervisorHandle {
    pub async fn command(&self, command: Command) -> Result<CommandOutput, CommandError> {
        let stops = matches!(command, Command::Stop);
        let (response, result) = oneshot::channel();
        self.tx
            .send(CommandRequest { command, response })
            .await
            .map_err(|_| CommandFailure::new(ProblemCode::EngineUnavailable))?;
        if stops {
            self.effect_cancel.cancel();
        }
        result
            .await
            .map_err(|_| CommandFailure::new(ProblemCode::EngineUnavailable))?
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorError {
    MissingIpc,
    IpcExited(ServiceExit),
    EngineExited(EngineExit),
    Startup(String),
    ShutdownTimedOut,
}

impl SupervisorError {
    pub fn failure(&self) -> CommandFailure {
        match self {
            Self::MissingIpc => CommandFailure::new(ProblemCode::StartupFailed)
                .with_detail("IPC service was not installed"),
            Self::IpcExited(exit) => CommandFailure::new(ProblemCode::StartupFailed)
                .with_detail(format!("IPC service exited: {exit:?}")),
            Self::EngineExited(exit) => engine_failure(exit),
            Self::Startup(detail) => {
                CommandFailure::new(ProblemCode::StartupFailed).with_detail(detail.clone())
            }
            Self::ShutdownTimedOut => CommandFailure::new(ProblemCode::ShutdownTimedOut),
        }
    }
}

fn engine_failure(exit: &EngineExit) -> CommandFailure {
    let code = match exit {
        EngineExit::ShutdownTimedOut { .. } => ProblemCode::ShutdownTimedOut,
        EngineExit::Cancelled | EngineExit::Completed => ProblemCode::EngineUnavailable,
        EngineExit::Service { .. } | EngineExit::Panicked(_) => ProblemCode::StartupFailed,
    };
    CommandFailure::new(code).with_detail(format!("{exit:?}"))
}

pub struct AppSupervisor {
    config: SupervisorConfig,
    clock: Arc<dyn Clock>,
    secrets: Arc<dyn SecretStore>,
    engine_port: Arc<dyn EnginePort>,
    gateway_factory: Arc<dyn GatewayFactory>,
    snapshots: Arc<SnapshotStore>,
    commands: mpsc::Receiver<CommandRequest>,
    ipc: Option<Box<dyn IpcPort>>,
    ipc_child: Option<ServiceChild>,
    engine: Option<EngineChild>,
    gateway: Option<Arc<dyn PlayitGateway>>,
    active_secret: Option<String>,
    next_refresh: Option<u64>,
    next_stats: u64,
    stopping: bool,
    terminal_error: Option<SupervisorError>,
    claims: ClaimService,
    active_claim: Option<ClaimSession>,
    effect_cancel: CancellationToken,
    process_shutdown: CancellationToken,
    start_attempt: u32,
}

impl AppSupervisor {
    pub fn new(
        config: SupervisorConfig,
        secrets: Arc<dyn SecretStore>,
        engine_port: Arc<dyn EnginePort>,
        clock: Arc<dyn Clock>,
    ) -> (Self, SupervisorHandle, Arc<SnapshotStore>) {
        Self::new_with_gateway_factory(
            config,
            secrets,
            engine_port,
            clock,
            Arc::new(GeneratedGatewayFactory),
        )
    }

    pub fn new_with_gateway_factory(
        config: SupervisorConfig,
        secrets: Arc<dyn SecretStore>,
        engine_port: Arc<dyn EnginePort>,
        clock: Arc<dyn Clock>,
        gateway_factory: Arc<dyn GatewayFactory>,
    ) -> (Self, SupervisorHandle, Arc<SnapshotStore>) {
        let mut initial = AppSnapshot::booting();
        initial.service = config.service.clone();
        let snapshots = Arc::new(SnapshotStore::new(initial));
        let (tx, commands) = mpsc::channel(32);
        let effect_cancel = CancellationToken::new();
        let handle = SupervisorHandle {
            tx,
            effect_cancel: effect_cancel.clone(),
        };
        let next_stats = config
            .policy
            .deadline_after(clock.now_millis(), config.policy.stats_interval);
        let claims = ClaimService::new(config.api_base.clone(), config.version.clone());
        (
            Self {
                config,
                clock,
                secrets,
                engine_port,
                gateway_factory,
                snapshots: snapshots.clone(),
                commands,
                ipc: None,
                ipc_child: None,
                engine: None,
                gateway: None,
                active_secret: None,
                next_refresh: None,
                next_stats,
                stopping: false,
                terminal_error: None,
                claims,
                active_claim: None,
                effect_cancel,
                process_shutdown: CancellationToken::new(),
                start_attempt: 1,
            },
            handle,
            snapshots,
        )
    }

    pub fn install_ipc(&mut self, ipc: Box<dyn IpcPort>) {
        self.ipc = Some(ipc);
    }

    pub async fn run(mut self, shutdown: CancellationToken) -> Result<(), SupervisorError> {
        self.process_shutdown = shutdown.clone();
        let ipc = self.ipc.take().ok_or(SupervisorError::MissingIpc)?;
        self.ipc_child = Some(ipc.start());
        self.apply_event(Event::Boot);
        if let Err(error) = self.initialize_secret().await {
            self.terminal_error = Some(error);
            self.begin_shutdown();
        }

        loop {
            if self.stopping {
                self.shutdown_children().await?;
                return match self.terminal_error.take() {
                    Some(error) => Err(error),
                    None => Ok(()),
                };
            }

            let wake = self.next_wake();
            let clock = self.clock.clone();
            let sleep = clock.sleep_until(wake);
            let engine_exit = self.engine.as_mut().map(|engine| &mut engine.exit);
            let ipc_exit = self.ipc_child.as_mut().map(|ipc| &mut ipc.exit);

            tokio::select! {
                request = self.commands.recv() => {
                    let Some(request) = request else {
                        self.begin_shutdown();
                        continue;
                    };
                    self.process_command(request).await;
                }
                _ = shutdown.cancelled() => self.begin_shutdown(),
                exit = wait_optional_child(engine_exit) => {
                    let exit = join_engine_exit(exit);
                    self.engine = None;
                    self.begin_shutdown();
                    self.shutdown_children().await?;
                    return Err(SupervisorError::EngineExited(exit));
                }
                exit = wait_optional_child(ipc_exit) => {
                    let exit = join_service_exit(exit);
                    self.ipc_child = None;
                    self.begin_shutdown();
                    self.shutdown_children().await?;
                    return Err(SupervisorError::IpcExited(exit));
                }
                _ = sleep => self.on_clock().await,
            }
        }
    }

    async fn initialize_secret(&mut self) -> Result<(), SupervisorError> {
        match self.secrets.load().await {
            SecretState::Ready(secret) => {
                self.active_secret = Some(secret);
                self.apply_event(Event::SecretLoaded);
                self.start_engine().await
            }
            SecretState::Missing => {
                self.apply_event(Event::SecretMissing);
                self.publish_problem(None, false);
                Ok(())
            }
            SecretState::Invalid(message) => {
                self.apply_event(Event::SecretInvalid);
                self.publish_problem(
                    Some(problem_report(ProblemCode::InvalidSecret, message.clone())),
                    false,
                );
                if self.secrets.can_provision() {
                    self.wait_for_replacement_secret();
                    Ok(())
                } else {
                    Err(SupervisorError::Startup(message))
                }
            }
        }
    }

    async fn process_command(&mut self, request: CommandRequest) {
        let CommandRequest { command, response } = request;
        let decision = decide_command(
            AppState {
                snapshot: (*self.snapshots.snapshot()).clone(),
            },
            command.clone(),
        );
        let (accepted_state, effects) = match decision {
            CommandDecision::Accepted { state, effects } => (state, effects),
            CommandDecision::Rejected { problem, .. } => {
                let mut failure = CommandFailure::new(problem.code);
                if let Phase::Blocked {
                    retry_at: Deadline::AtMillis(retry_at),
                    ..
                } = self.snapshots.snapshot().phase
                {
                    failure = failure.with_retry_at(retry_at);
                }
                let _ = response.send(Err(failure));
                return;
            }
        };
        let publish_after_effect = matches!(command, Command::ResetSecret);
        if !publish_after_effect {
            self.snapshots.apply_state(accepted_state.clone());
        }
        let can_start_engine = matches!(
            command,
            Command::ProvisionSecret { .. } | Command::ExchangeClaim { .. }
        );
        let result = self.execute_command_effects(command, effects).await;
        if publish_after_effect && result.is_ok() {
            self.snapshots.apply_state(accepted_state);
        }
        let start_after_response = can_start_engine
            && matches!(
                result,
                Ok(CommandOutput::Accepted | CommandOutput::ClaimProvisioned)
            );
        let _ = response.send(result);
        if start_after_response && let Err(error) = self.start_engine().await {
            self.terminal_error = Some(error);
            self.begin_shutdown();
        }
    }

    async fn execute_command_effects(
        &mut self,
        command: Command,
        effects: Vec<Effect>,
    ) -> Result<CommandOutput, CommandError> {
        match command {
            Command::BeginClaim if effects.contains(&Effect::StartClaim) => {
                let session = ClaimService::begin();
                self.active_claim = Some(session.clone());
                Ok(CommandOutput::ClaimStarted(session))
            }
            Command::PollClaim { code } if effects.contains(&Effect::PollClaim) => {
                self.ensure_active_claim(&code)?;
                self.bounded_gateway(self.claims.progress(&code, ClaimMode::Assignable))
                    .await
                    .map_err(bounded_command_error)?
                    .map(CommandOutput::ClaimProgress)
                    .map_err(claim_error)
            }
            Command::ExchangeClaim { code } if effects.contains(&Effect::ExchangeClaim) => {
                self.ensure_active_claim(&code)?;
                let exchange = self
                    .bounded_gateway(self.claims.exchange(&code))
                    .await
                    .map_err(bounded_command_error)?
                    .map_err(claim_error)?;
                match exchange {
                    ClaimExchange::Pending(status) => Ok(CommandOutput::ClaimPending(status)),
                    ClaimExchange::Accepted(secret) => {
                        let secret = SecretInput::new(secret).map_err(|_| {
                            CommandFailure::new(ProblemCode::InvalidSecret)
                                .with_detail("claim exchange returned an empty secret")
                        })?;
                        self.apply_event(Event::SecretLoaded);
                        self.persist_provisioned_secret(&secret).await?;
                        self.active_claim = None;
                        Ok(CommandOutput::ClaimProvisioned)
                    }
                }
            }
            Command::ProvisionSecret { secret }
                if effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::PersistSecret(_))) =>
            {
                self.persist_provisioned_secret(&secret).await?;
                Ok(CommandOutput::Accepted)
            }
            Command::ResetSecret if effects.contains(&Effect::ResetSecret) => {
                let reset = self.secrets.reset().await.map_err(secret_error)?;
                self.stopping = true;
                Ok(CommandOutput::SecretReset(reset))
            }
            Command::CreateLoginUrl if effects.contains(&Effect::CreateLoginUrl) => {
                let gateway = self
                    .gateway
                    .as_ref()
                    .ok_or_else(|| CommandFailure::new(ProblemCode::EngineUnavailable))?;
                self.bounded_gateway(gateway.account_login_url())
                    .await
                    .map_err(bounded_command_error)?
                    .map(CommandOutput::LoginUrl)
                    .map_err(|error| {
                        CommandFailure::new(error.problem_code()).with_detail(error.to_string())
                    })
            }
            Command::RefreshCatalog if effects.contains(&Effect::FetchCatalog) => {
                self.refresh_catalog().await;
                Ok(CommandOutput::Accepted)
            }
            Command::Stop if effects.contains(&Effect::StopEngine) => {
                self.stopping = true;
                Ok(CommandOutput::Accepted)
            }
            _ => Err(CommandFailure::new(ProblemCode::CommandNotAllowed)),
        }
    }

    async fn start_engine(&mut self) -> Result<(), SupervisorError> {
        let secret = self
            .active_secret
            .clone()
            .ok_or_else(|| SupervisorError::Startup("agent secret is unavailable".to_owned()))?;
        let gateway = self.gateway_factory.create(&self.config.api_base, &secret);
        let initial_data = match self
            .bounded_gateway(gateway.run_data(self.run_data_context()))
            .await
        {
            Ok(result) => result.ok(),
            Err(BoundedError::Cancelled) => return Ok(()),
            Err(BoundedError::TimedOut) => None,
        };
        let origins = initial_data
            .as_ref()
            .map(|data| data.origins.as_slice())
            .unwrap_or_default();

        let engine_start = self
            .bounded_gateway(self.engine_port.start(gateway.clone(), origins))
            .await;
        let engine_start = match engine_start {
            Ok(result) => result,
            Err(BoundedError::Cancelled) => return Ok(()),
            Err(BoundedError::TimedOut) => Err(EngineStartError::Failed(
                "engine startup timed out".to_owned(),
            )),
        };

        match engine_start {
            Ok(engine) => {
                self.engine = Some(engine);
                self.gateway = Some(gateway);
                if let Some(data) = initial_data {
                    self.publish_run_data(data).await;
                } else {
                    self.next_refresh = Some(self.clock.now_millis());
                }
                Ok(())
            }
            Err(EngineStartError::InvalidSecret(message)) => {
                self.gateway = Some(gateway);
                self.apply_event(Event::SecretInvalid);
                self.publish_problem(
                    Some(problem_report(ProblemCode::InvalidSecret, message)),
                    false,
                );
                if self.secrets.can_provision() {
                    self.wait_for_replacement_secret();
                    Ok(())
                } else {
                    Err(SupervisorError::Startup(
                        "the configured secret is invalid".to_owned(),
                    ))
                }
            }
            Err(EngineStartError::AgentDisabledOverLimit(message)) => {
                self.gateway = Some(gateway);
                let retry_at = self.config.policy.retry_deadline(
                    self.clock.now_millis(),
                    GatewayErrorCode::AgentDisabledOverLimit,
                );
                self.apply_event(Event::EngineStartFailed {
                    problem: Problem::new(ProblemCode::AgentDisabledOverLimit),
                    retry_at: Deadline::AtMillis(retry_at),
                });
                self.publish_problem(
                    Some(problem_report(ProblemCode::AgentDisabledOverLimit, message)),
                    true,
                );
                self.next_refresh = Some(retry_at);
                Ok(())
            }
            Err(EngineStartError::Failed(message)) => {
                self.apply_event(Event::EngineStartFailed {
                    problem: Problem::new(ProblemCode::Internal),
                    retry_at: Deadline::Unscheduled,
                });
                self.publish_problem(
                    Some(problem_report(ProblemCode::Internal, message.clone())),
                    true,
                );
                Err(SupervisorError::Startup(message))
            }
        }
    }

    async fn on_clock(&mut self) {
        let now = self.clock.now_millis();
        self.snapshots.publish(|snapshot| {
            snapshot.service.uptime_secs = now.saturating_sub(self.config.start_time) / 1_000;
        });

        if now >= self.next_stats {
            if let Some(engine) = &self.engine {
                let traffic = engine.traffic.snapshot();
                self.snapshots
                    .publish(|snapshot| snapshot.traffic = traffic);
            }
            self.next_stats = self
                .config
                .policy
                .deadline_after(now, self.config.policy.stats_interval);
        }

        if self.next_refresh.is_some_and(|deadline| now >= deadline) {
            if matches!(self.snapshots.snapshot().phase, Phase::Blocked { .. }) {
                self.start_attempt = self.start_attempt.saturating_add(1);
                self.apply_event(Event::RetryElapsed {
                    attempt: self.start_attempt,
                });
                if let Err(error) = self.start_engine().await {
                    tracing::error!(?error, "agent retry failed");
                }
            } else {
                self.refresh_catalog().await;
            }
        }
    }

    async fn refresh_catalog(&mut self) {
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        let response = self
            .bounded_gateway(gateway.run_data(self.run_data_context()))
            .await;
        match response {
            Err(BoundedError::Cancelled) => {}
            Err(BoundedError::TimedOut) => {
                let retry_at = self
                    .config
                    .policy
                    .retry_deadline(self.clock.now_millis(), GatewayErrorCode::Transport);
                self.snapshots.publish(|snapshot| {
                    snapshot.catalog.last_problem =
                        Some(Problem::new(ProblemCode::CatalogUnavailable));
                });
                self.next_refresh = Some(retry_at);
                tracing::warn!("catalog refresh timed out");
            }
            Ok(Ok(data)) => self.publish_run_data(data).await,
            Ok(Err(error)) => {
                let retry_at = self
                    .config
                    .policy
                    .retry_deadline(self.clock.now_millis(), error.code);
                if matches!(self.snapshots.snapshot().phase, Phase::Online { .. }) {
                    self.apply_event(Event::CatalogLoadFailed {
                        retry_at: Deadline::AtMillis(retry_at),
                    });
                } else {
                    self.snapshots.publish(|snapshot| {
                        snapshot.catalog.last_problem = Some(error.problem());
                    });
                }
                self.next_refresh = Some(retry_at);
                tracing::error!(?error, "failed to load agent data");
            }
        }
    }

    fn ensure_active_claim(&self, code: &str) -> Result<(), CommandError> {
        self.active_claim
            .as_ref()
            .filter(|session| session.code == code)
            .map(|_| ())
            .ok_or_else(|| {
                CommandFailure::new(ProblemCode::CommandNotAllowed)
                    .with_detail("claim session is not active")
            })
    }

    async fn persist_provisioned_secret(
        &mut self,
        secret: &SecretInput,
    ) -> Result<(), CommandError> {
        if validate_secret(secret.expose()).is_err() {
            self.apply_event(Event::SecretPersistenceFailed);
            return Err(CommandFailure::new(ProblemCode::InvalidSecret)
                .with_detail("secret is not valid hexadecimal"));
        }
        if let Err(error) = self.secrets.persist(secret).await {
            self.apply_event(Event::SecretPersistenceFailed);
            return Err(secret_error(error));
        }
        self.active_secret = Some(secret.expose().to_owned());
        self.start_attempt = 1;
        self.apply_event(Event::SecretPersisted);
        Ok(())
    }

    fn bounded_gateway<T>(
        &self,
        future: impl Future<Output = T> + Send,
    ) -> impl Future<Output = Result<T, BoundedError>> + Send {
        let effect_cancel = self.effect_cancel.clone();
        let process_shutdown = self.process_shutdown.clone();
        let timeout = self.config.policy.gateway_timeout;
        async move {
            tokio::select! {
                biased;
                _ = effect_cancel.cancelled() => Err(BoundedError::Cancelled),
                _ = process_shutdown.cancelled() => Err(BoundedError::Cancelled),
                result = tokio::time::timeout(timeout, future) => {
                    result.map_err(|_| BoundedError::TimedOut)
                }
            }
        }
    }

    fn run_data_context(&self) -> RunDataContext {
        let snapshot = self.snapshots.snapshot();
        RunDataContext {
            catalog_revision: snapshot.catalog.accepted.revision().saturating_add(1),
            accepted_at_millis: self.clock.now_millis(),
        }
    }

    async fn publish_run_data(&mut self, data: GatewayRunData) {
        if let Some(engine) = &self.engine {
            engine.origins.replace(&data.origins).await;
        }
        if !matches!(self.snapshots.snapshot().phase, Phase::Online { .. }) {
            self.apply_event(Event::EngineStarted {
                agent: AgentIdentity::new(data.agent_id.clone()),
            });
        }
        self.apply_event(Event::CatalogLoaded {
            catalog: data.catalog.clone(),
        });
        self.snapshots.publish(move |snapshot| {
            snapshot.catalog.pending = data.pending;
            snapshot.catalog.account_status = data.account_status;
            snapshot.catalog.login_url = data.login_url;
            snapshot.catalog.last_problem = None;
            snapshot.notices = data.notices;
            snapshot.last_problem = None;
            snapshot.service.has_secret = true;
        });
        self.next_refresh = Some(
            self.config
                .policy
                .deadline_after(self.clock.now_millis(), self.config.policy.refresh_interval),
        );
    }

    fn apply_event(&self, event: Event) {
        let state = AppState {
            snapshot: (*self.snapshots.snapshot()).clone(),
        };
        if let EventDecision::Applied { state, .. } = reduce(state, event) {
            self.snapshots.apply_state(state);
        }
    }

    fn publish_problem(&self, problem: Option<ProblemReport>, has_secret: bool) {
        self.snapshots.publish(|snapshot| {
            snapshot.last_problem = problem;
            snapshot.service.has_secret = has_secret;
        });
    }

    fn wait_for_replacement_secret(&self) {
        self.snapshots.publish(|snapshot| {
            snapshot.phase = Phase::NeedsSecret {
                reason: playit_model::SecretNeed::Missing,
            };
        });
    }

    fn next_wake(&self) -> u64 {
        self.next_refresh
            .map(|refresh| refresh.min(self.next_stats))
            .unwrap_or(self.next_stats)
    }

    fn begin_shutdown(&mut self) {
        if self.stopping {
            return;
        }
        let decision = decide_command(
            AppState {
                snapshot: (*self.snapshots.snapshot()).clone(),
            },
            Command::Stop,
        );
        if let CommandDecision::Accepted { state, .. } = decision {
            self.snapshots.apply_state(state);
        } else {
            self.snapshots
                .publish(|snapshot| snapshot.phase = Phase::Stopping);
        }
        self.stopping = true;
    }

    async fn shutdown_children(&mut self) -> Result<(), SupervisorError> {
        let deadline = self.config.policy.deadline_after(
            self.clock.now_millis(),
            self.config.policy.shutdown_deadline,
        );
        if let Some(engine) = self.engine.as_ref() {
            engine.cancel.cancel();
        }
        let engine_exit = self.engine.as_mut().map(|engine| &mut engine.exit);
        if !await_child(engine_exit, self.clock.clone(), deadline).await {
            if let Some(engine) = self.engine.take() {
                engine.exit.abort();
                let _ = engine.exit.await;
            }
            if let Some(ipc) = self.ipc_child.as_ref() {
                ipc.cancel.cancel();
            }
            if let Some(ipc) = self.ipc_child.take() {
                ipc.exit.abort();
                let _ = ipc.exit.await;
            }
            self.finish_shutdown(true);
            return Err(SupervisorError::ShutdownTimedOut);
        }
        self.engine = None;

        if let Some(ipc) = self.ipc_child.as_ref() {
            ipc.cancel.cancel();
        }
        let ipc_exit = self.ipc_child.as_mut().map(|ipc| &mut ipc.exit);
        if !await_child(ipc_exit, self.clock.clone(), deadline).await {
            if let Some(ipc) = self.ipc_child.take() {
                ipc.exit.abort();
                let _ = ipc.exit.await;
            }
            self.finish_shutdown(true);
            return Err(SupervisorError::ShutdownTimedOut);
        }
        self.ipc_child = None;
        self.finish_shutdown(false);
        Ok(())
    }

    fn finish_shutdown(&self, timed_out: bool) {
        self.apply_event(if timed_out {
            Event::ShutdownTimedOut
        } else {
            Event::ShutdownComplete
        });
    }
}

async fn wait_optional_child<T>(
    child: Option<&mut JoinHandle<T>>,
) -> Result<T, tokio::task::JoinError> {
    match child {
        Some(child) => child.await,
        None => std::future::pending().await,
    }
}

async fn await_child<T>(
    child: Option<&mut JoinHandle<T>>,
    clock: Arc<dyn Clock>,
    deadline: u64,
) -> bool {
    let Some(child) = child else {
        return true;
    };
    tokio::select! {
        _ = child => true,
        _ = clock.sleep_until(deadline) => false,
    }
}

fn join_service_exit(result: Result<ServiceExit, tokio::task::JoinError>) -> ServiceExit {
    match result {
        Ok(exit) => exit,
        Err(error) => ServiceExit::Panicked(error.to_string()),
    }
}

fn join_engine_exit(result: Result<EngineExit, tokio::task::JoinError>) -> EngineExit {
    match result {
        Ok(exit) => exit,
        Err(error) => EngineExit::Panicked(error.to_string()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundedError {
    Cancelled,
    TimedOut,
}

fn bounded_command_error(error: BoundedError) -> CommandError {
    let detail = match error {
        BoundedError::Cancelled => "request cancelled because the service is stopping",
        BoundedError::TimedOut => "gateway request timed out",
    };
    CommandFailure::new(ProblemCode::EngineUnavailable).with_detail(detail)
}

fn claim_error(error: ClaimFailure) -> CommandError {
    CommandFailure {
        problem: error.problem,
        detail: Some(error.detail),
        retry_at_millis: None,
    }
}

fn secret_error(error: SecretStoreError) -> CommandError {
    match error {
        SecretStoreError::Pinned => CommandFailure::new(ProblemCode::SecretPinned),
        SecretStoreError::Io(message) => {
            CommandFailure::new(ProblemCode::SecretWriteFailed).with_detail(message)
        }
    }
}

fn problem_report(code: ProblemCode, detail: String) -> ProblemReport {
    ProblemReport {
        problem: Problem::new(code),
        detail,
    }
}

fn parse_secret_file(content: &str) -> Result<String, ()> {
    let trimmed = content.trim();
    if let Ok(secret) = validate_secret(trimmed) {
        return Ok(secret);
    }
    let config = toml::from_str::<SecretConfig>(content).map_err(|_| ())?;
    validate_secret(config.secret_key.trim()).map_err(|_| ())
}

fn validate_secret(secret: &str) -> Result<String, String> {
    hex::decode(secret)
        .map(|_| secret.to_owned())
        .map_err(|_| "secret is not valid hexadecimal".to_owned())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SecretConfig {
    secret_key: String,
}

async fn persist_secret_file(path: &Path, secret: &str) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            format!(
                "Failed to create secret directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let content = if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        toml::to_string(&SecretConfig {
            secret_key: secret.to_owned(),
        })
        .map_err(|error| {
            format!(
                "Failed to serialize secret file {}: {error}",
                path.display()
            )
        })?
    } else {
        secret.to_owned()
    };
    playit_platform::secret::atomic_write_secret(path, content.as_bytes())
        .await
        .map_err(|error| format!("Failed to write secret file {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use playit_model::{AccountStatus, TunnelCatalog};

    use crate::ClaimGateway;
    use playit_agent_core::gateway::{GatewayError, RegisterRequest, SignedAgentKey};
    use playit_agent_core::playit_agent::EngineService;

    struct DropTracker(Arc<AtomicBool>);

    impl Drop for DropTracker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    struct MissingSecrets;

    #[async_trait]
    impl SecretStore for MissingSecrets {
        async fn load(&self) -> SecretState {
            SecretState::Missing
        }

        async fn persist(&self, _secret: &SecretInput) -> Result<(), SecretStoreError> {
            Ok(())
        }

        async fn reset(&self) -> Result<SecretReset, SecretStoreError> {
            Ok(SecretReset::AlreadyAbsent(PathBuf::from("fixture")))
        }

        fn path(&self) -> Option<&Path> {
            Some(Path::new("fixture"))
        }

        fn can_provision(&self) -> bool {
            true
        }
    }

    struct UnusedEngine;

    #[async_trait]
    impl EnginePort for UnusedEngine {
        async fn start(
            &self,
            _gateway: Arc<dyn PlayitGateway>,
            _origins: &[GatewayOrigin],
        ) -> Result<EngineChild, EngineStartError> {
            panic!("missing-secret tests must not start the engine")
        }
    }

    struct HoldingEngine;

    #[async_trait]
    impl EnginePort for HoldingEngine {
        async fn start(
            &self,
            _gateway: Arc<dyn PlayitGateway>,
            _origins: &[GatewayOrigin],
        ) -> Result<EngineChild, EngineStartError> {
            let cancel = CancellationToken::new();
            let wait = cancel.clone();
            Ok(EngineChild::new(
                cancel,
                tokio::spawn(async move {
                    wait.cancelled().await;
                    EngineExit::Cancelled
                }),
                Arc::new(NoopOrigins),
                Arc::new(NoopTraffic),
            ))
        }
    }

    struct ReadySecrets;

    #[async_trait]
    impl SecretStore for ReadySecrets {
        async fn load(&self) -> SecretState {
            SecretState::Ready("00".to_owned())
        }

        async fn persist(&self, _secret: &SecretInput) -> Result<(), SecretStoreError> {
            Ok(())
        }

        async fn reset(&self) -> Result<SecretReset, SecretStoreError> {
            Ok(SecretReset::AlreadyAbsent(PathBuf::from("fixture")))
        }

        fn path(&self) -> Option<&Path> {
            Some(Path::new("fixture"))
        }

        fn can_provision(&self) -> bool {
            true
        }
    }

    struct FakeGateway {
        run_data: Mutex<VecDeque<Result<GatewayRunData, GatewayError>>>,
    }

    #[async_trait]
    impl PlayitGateway for FakeGateway {
        async fn register(
            &self,
            _request: RegisterRequest,
        ) -> Result<SignedAgentKey, GatewayError> {
            unreachable!()
        }

        async fn control_addresses(&self) -> Result<Vec<std::net::SocketAddr>, GatewayError> {
            unreachable!()
        }

        async fn run_data(&self, _context: RunDataContext) -> Result<GatewayRunData, GatewayError> {
            self.run_data
                .lock()
                .unwrap()
                .pop_front()
                .expect("fixture run-data response")
        }

        async fn account_login_url(&self) -> Result<String, GatewayError> {
            Ok("https://example.invalid/login".to_owned())
        }
    }

    struct FakeGatewayFactory(Arc<FakeGateway>);

    impl GatewayFactory for FakeGatewayFactory {
        fn create(&self, _api_base: &str, _secret: &str) -> Arc<dyn PlayitGateway> {
            self.0.clone()
        }
    }

    struct HangingGateway;

    #[async_trait]
    impl PlayitGateway for HangingGateway {
        async fn register(
            &self,
            _request: RegisterRequest,
        ) -> Result<SignedAgentKey, GatewayError> {
            std::future::pending().await
        }

        async fn control_addresses(&self) -> Result<Vec<std::net::SocketAddr>, GatewayError> {
            std::future::pending().await
        }

        async fn run_data(&self, _context: RunDataContext) -> Result<GatewayRunData, GatewayError> {
            std::future::pending().await
        }

        async fn account_login_url(&self) -> Result<String, GatewayError> {
            std::future::pending().await
        }
    }

    struct HangingGatewayFactory;

    impl GatewayFactory for HangingGatewayFactory {
        fn create(&self, _api_base: &str, _secret: &str) -> Arc<dyn PlayitGateway> {
            Arc::new(HangingGateway)
        }
    }

    struct AcceptedClaimGateway;

    #[async_trait]
    impl ClaimGateway for AcceptedClaimGateway {
        async fn progress(
            &self,
            _code: &str,
            _mode: ClaimMode,
            _version: &str,
        ) -> Result<ClaimProgress, ClaimFailure> {
            Ok(ClaimProgress::Approved)
        }

        async fn exchange(&self, _code: &str) -> Result<ClaimExchange, ClaimFailure> {
            Ok(ClaimExchange::Accepted("00".to_owned()))
        }
    }

    struct ImmediateIpcExit;

    impl IpcPort for ImmediateIpcExit {
        fn start(self: Box<Self>) -> ServiceChild {
            ServiceChild::new(
                CancellationToken::new(),
                tokio::spawn(async { ServiceExit::Failed("fixture IPC exit".to_owned()) }),
            )
        }
    }

    struct CancelledIpc;

    impl IpcPort for CancelledIpc {
        fn start(self: Box<Self>) -> ServiceChild {
            let cancel = CancellationToken::new();
            let child_cancel = cancel.clone();
            ServiceChild::new(
                cancel,
                tokio::spawn(async move {
                    child_cancel.cancelled().await;
                    ServiceExit::Cancelled
                }),
            )
        }
    }

    fn test_supervisor(policy: SupervisorPolicy) -> (AppSupervisor, SupervisorHandle) {
        let config = SupervisorConfig {
            api_base: "https://example.invalid".to_owned(),
            version: "1.0.10".to_owned(),
            start_time: 0,
            service: ServiceInfo::default(),
            policy,
        };
        let (supervisor, handle, _) = AppSupervisor::new(
            config,
            Arc::new(MissingSecrets),
            Arc::new(UnusedEngine),
            Arc::new(SystemClock),
        );
        (supervisor, handle)
    }

    #[test]
    fn retry_schedule_uses_policy_and_saturates() {
        let policy = SupervisorPolicy::default();
        assert_eq!(
            policy.retry_deadline(1_000, GatewayErrorCode::AgentDisabledOverLimit),
            31_000
        );
        assert_eq!(
            policy.retry_deadline(1_000, GatewayErrorCode::Transport),
            4_000
        );
        assert_eq!(
            policy.deadline_after(u64::MAX - 1, Duration::from_secs(1)),
            u64::MAX
        );
    }

    #[test]
    fn clock_changes_create_deadlines_from_the_latest_reading() {
        let policy = SupervisorPolicy::default();
        assert_eq!(
            policy.retry_deadline(50_000, GatewayErrorCode::Transport),
            53_000
        );
        assert_eq!(
            policy.retry_deadline(5_000, GatewayErrorCode::Transport),
            8_000
        );
    }

    #[test]
    fn snapshot_publication_is_atomic_and_monotonic() {
        let store = SnapshotStore::new(AppSnapshot::booting());
        store.publish(|snapshot| {
            snapshot.phase = Phase::Starting { attempt: 1 };
            snapshot.traffic.bytes_in = 5;
        });
        let snapshot = store.snapshot();
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.traffic.bytes_in, 5);
    }

    #[test]
    fn supervisor_future_can_be_owned_by_a_process_task() {
        fn assert_send<T: Send>(_value: T) {}

        let (mut supervisor, _handle) = test_supervisor(SupervisorPolicy::default());
        supervisor.install_ipc(Box::new(CancelledIpc));
        assert_send(supervisor.run(CancellationToken::new()));
    }

    #[tokio::test]
    async fn typed_stop_command_drives_supervisor_shutdown() {
        let (mut supervisor, handle) = test_supervisor(SupervisorPolicy::default());
        supervisor.install_ipc(Box::new(CancelledIpc));
        let command = async move {
            tokio::task::yield_now().await;
            handle.command(Command::Stop).await
        };
        let (run_result, command_result) =
            tokio::join!(supervisor.run(CancellationToken::new()), command);
        assert_eq!(command_result, Ok(CommandOutput::Accepted));
        assert_eq!(run_result, Ok(()));
    }

    #[tokio::test]
    async fn stop_interrupts_an_in_flight_gateway_request() {
        let policy = SupervisorPolicy {
            gateway_timeout: Duration::from_secs(60),
            ..SupervisorPolicy::default()
        };
        let config = SupervisorConfig {
            api_base: "https://example.invalid".to_owned(),
            version: "1.0.10".to_owned(),
            start_time: 0,
            service: ServiceInfo::default(),
            policy,
        };
        let (mut supervisor, handle, _) = AppSupervisor::new_with_gateway_factory(
            config,
            Arc::new(ReadySecrets),
            Arc::new(HoldingEngine),
            Arc::new(SystemClock),
            Arc::new(HangingGatewayFactory),
        );
        supervisor.install_ipc(Box::new(CancelledIpc));

        let command = async move {
            tokio::task::yield_now().await;
            handle.command(Command::Stop).await
        };
        let result = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(supervisor.run(CancellationToken::new()), command)
        })
        .await
        .expect("stop must cancel the gateway request without waiting for its timeout");

        assert_eq!(result.0, Ok(()));
        assert_eq!(result.1, Ok(CommandOutput::Accepted));
    }

    #[tokio::test]
    async fn failed_secret_reset_preserves_the_running_snapshot() {
        let (mut supervisor, _handle) = test_supervisor(SupervisorPolicy::default());
        supervisor.secrets = Arc::new(InlineSecretStore::new("00".to_owned()));
        supervisor.snapshots.publish(|snapshot| {
            snapshot.phase = Phase::Online {
                agent: AgentIdentity::new("agent-1"),
            };
        });
        let before = supervisor.snapshots.snapshot();
        let (response, result) = oneshot::channel();

        supervisor
            .process_command(CommandRequest {
                command: Command::ResetSecret,
                response,
            })
            .await;

        assert_eq!(
            result.await,
            Ok(Err(CommandFailure::new(ProblemCode::SecretPinned)))
        );
        assert_eq!(supervisor.snapshots.snapshot(), before);
        assert!(!supervisor.stopping);
    }

    #[tokio::test]
    async fn typed_begin_claim_command_owns_setup_session_creation() {
        let (mut supervisor, handle) = test_supervisor(SupervisorPolicy::default());
        supervisor.install_ipc(Box::new(CancelledIpc));
        let command = async move {
            tokio::task::yield_now().await;
            let output = handle.command(Command::BeginClaim).await.unwrap();
            let CommandOutput::ClaimStarted(session) = output else {
                panic!("begin claim returned the wrong command output");
            };
            assert_eq!(session.code.len(), 10);
            assert!(session.url.ends_with(&session.code));
            assert_eq!(
                handle
                    .command(Command::PollClaim {
                        code: "not-the-active-code".to_owned(),
                    })
                    .await,
                Err(CommandFailure::new(ProblemCode::CommandNotAllowed)
                    .with_detail("claim session is not active"))
            );
            handle.command(Command::Stop).await
        };
        let (run_result, stop_result) =
            tokio::join!(supervisor.run(CancellationToken::new()), command);
        assert_eq!(stop_result, Ok(CommandOutput::Accepted));
        assert_eq!(run_result, Ok(()));
    }

    #[tokio::test]
    async fn accepted_daemon_claim_persists_without_returning_the_secret() {
        let (mut supervisor, _handle) = test_supervisor(SupervisorPolicy::default());
        supervisor.snapshots.publish(|snapshot| {
            snapshot.phase = Phase::NeedsSecret {
                reason: playit_model::SecretNeed::Missing,
            };
        });
        supervisor.claims = ClaimService::with_gateway(
            Arc::new(AcceptedClaimGateway),
            "fixture-version".to_owned(),
        );
        supervisor.active_claim = Some(ClaimSession {
            code: "active-code".to_owned(),
            url: "https://playit.gg/claim/active-code".to_owned(),
        });

        let output = supervisor
            .execute_command_effects(
                Command::ExchangeClaim {
                    code: "active-code".to_owned(),
                },
                vec![Effect::ExchangeClaim],
            )
            .await;

        assert_eq!(output, Ok(CommandOutput::ClaimProvisioned));
        assert_eq!(supervisor.active_secret.as_deref(), Some("00"));
        assert!(supervisor.active_claim.is_none());
        assert!(matches!(
            supervisor.snapshots.snapshot().phase,
            Phase::Starting { attempt: 1 }
        ));
    }

    #[tokio::test]
    async fn unexpected_ipc_exit_reaches_the_supervisor() {
        let (mut supervisor, _handle) = test_supervisor(SupervisorPolicy::default());
        supervisor.install_ipc(Box::new(ImmediateIpcExit));

        let result = supervisor.run(CancellationToken::new()).await;
        assert_eq!(
            result,
            Err(SupervisorError::IpcExited(ServiceExit::Failed(
                "fixture IPC exit".to_owned()
            )))
        );
    }

    #[tokio::test]
    async fn unexpected_engine_child_exit_reaches_the_supervisor() {
        let (mut supervisor, _handle) = test_supervisor(SupervisorPolicy::default());
        supervisor.install_ipc(Box::new(CancelledIpc));
        let cancel = CancellationToken::new();
        let expected = EngineExit::Service {
            service: EngineService::Tcp,
            exit: ServiceExit::Failed("fixture TCP child failure".to_owned()),
        };
        let child_exit = expected.clone();
        supervisor.engine = Some(EngineChild::new(
            cancel,
            tokio::spawn(async move { child_exit }),
            Arc::new(NoopOrigins),
            Arc::new(NoopTraffic),
        ));

        assert_eq!(
            supervisor.run(CancellationToken::new()).await,
            Err(SupervisorError::EngineExited(expected))
        );
    }

    #[tokio::test]
    async fn shutdown_waits_for_engine_before_stopping_ipc() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (mut supervisor, _handle) = test_supervisor(SupervisorPolicy::default());
        supervisor
            .snapshots
            .publish(|snapshot| snapshot.phase = Phase::Stopping);

        let engine_cancel = CancellationToken::new();
        let engine_wait = engine_cancel.clone();
        let engine_events = events.clone();
        let engine_exit = tokio::spawn(async move {
            engine_wait.cancelled().await;
            engine_events.lock().unwrap().push("engine");
            EngineExit::Cancelled
        });
        supervisor.engine = Some(EngineChild::new(
            engine_cancel,
            engine_exit,
            Arc::new(NoopOrigins),
            Arc::new(NoopTraffic),
        ));

        let ipc_cancel = CancellationToken::new();
        let ipc_wait = ipc_cancel.clone();
        let ipc_events = events.clone();
        let ipc_exit = tokio::spawn(async move {
            ipc_wait.cancelled().await;
            ipc_events.lock().unwrap().push("ipc");
            ServiceExit::Cancelled
        });
        supervisor.ipc_child = Some(ServiceChild::new(ipc_cancel, ipc_exit));

        assert_eq!(supervisor.shutdown_children().await, Ok(()));
        assert_eq!(events.lock().unwrap().as_slice(), ["engine", "ipc"]);
    }

    #[tokio::test]
    async fn shutdown_deadline_reports_a_typed_timeout() {
        let policy = SupervisorPolicy {
            shutdown_deadline: Duration::from_millis(5),
            ..SupervisorPolicy::default()
        };
        let (mut supervisor, _handle) = test_supervisor(policy);
        supervisor
            .snapshots
            .publish(|snapshot| snapshot.phase = Phase::Stopping);
        let cancel = CancellationToken::new();
        let engine_dropped = Arc::new(AtomicBool::new(false));
        let tracker = DropTracker(engine_dropped.clone());
        supervisor.engine = Some(EngineChild::new(
            cancel,
            tokio::spawn(async move {
                let _tracker = tracker;
                std::future::pending().await
            }),
            Arc::new(NoopOrigins),
            Arc::new(NoopTraffic),
        ));

        assert_eq!(
            supervisor.shutdown_children().await,
            Err(SupervisorError::ShutdownTimedOut)
        );
        assert!(engine_dropped.load(Ordering::Acquire));
        assert!(supervisor.engine.is_none());
        assert_eq!(
            supervisor
                .snapshots
                .snapshot()
                .notices
                .last()
                .unwrap()
                .problem
                .code,
            ProblemCode::ShutdownTimedOut
        );
    }

    #[tokio::test]
    async fn refresh_failure_retains_the_accepted_catalog() {
        let catalog = TunnelCatalog::try_new(7, 100, Vec::new()).unwrap();
        let gateway = Arc::new(FakeGateway {
            run_data: Mutex::new(VecDeque::from([
                Ok(GatewayRunData {
                    agent_id: "agent-1".to_owned(),
                    origins: Vec::new(),
                    catalog: catalog.clone(),
                    account_status: AccountStatus::Verified,
                    pending: Vec::new(),
                    notices: Vec::new(),
                    login_url: None,
                }),
                Err(GatewayError::new(
                    GatewayErrorCode::Transport,
                    "run_data",
                    "fixture failure",
                )),
            ])),
        });
        let config = SupervisorConfig {
            api_base: "https://example.invalid".to_owned(),
            version: "1.0.10".to_owned(),
            start_time: 0,
            service: ServiceInfo::default(),
            policy: SupervisorPolicy::default(),
        };
        let (mut supervisor, handle, snapshots) = AppSupervisor::new_with_gateway_factory(
            config,
            Arc::new(ReadySecrets),
            Arc::new(HoldingEngine),
            Arc::new(SystemClock),
            Arc::new(FakeGatewayFactory(gateway)),
        );
        supervisor.install_ipc(Box::new(CancelledIpc));
        let commands = async move {
            tokio::task::yield_now().await;
            assert_eq!(
                handle.command(Command::RefreshCatalog).await,
                Ok(CommandOutput::Accepted)
            );
            assert_eq!(snapshots.snapshot().catalog.accepted.revision(), 7);
            assert_eq!(
                snapshots
                    .snapshot()
                    .catalog
                    .last_problem
                    .as_ref()
                    .unwrap()
                    .code,
                ProblemCode::CatalogUnavailable
            );
            handle.command(Command::Stop).await
        };
        let (run_result, stop_result) =
            tokio::join!(supervisor.run(CancellationToken::new()), commands);
        assert_eq!(stop_result, Ok(CommandOutput::Accepted));
        assert_eq!(run_result, Ok(()));
    }

    struct NoopOrigins;

    #[async_trait]
    impl OriginPublisher for NoopOrigins {
        async fn replace(&self, _origins: &[GatewayOrigin]) {}
    }

    struct NoopTraffic;

    impl TrafficSource for NoopTraffic {
        fn snapshot(&self) -> TrafficSnapshot {
            TrafficSnapshot::default()
        }
    }
}
