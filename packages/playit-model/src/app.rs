use std::fmt;

use crate::problem::{Problem, ProblemCode};
use crate::tunnel::TunnelCatalog;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretNeed {
    Missing,
    Invalid,
    PersistenceFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deadline {
    AtMillis(u64),
    Unscheduled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    id: String,
}

impl AgentIdentity {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }

    pub fn as_str(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Booting,
    NeedsSecret {
        reason: SecretNeed,
    },
    Starting {
        attempt: u32,
    },
    Blocked {
        problem: Problem,
        retry_at: Deadline,
    },
    Online {
        agent: AgentIdentity,
    },
    Stopping,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceInfo {
    pub process_id: Option<u32>,
    pub uptime_secs: u64,
    pub started_at_millis: u64,
    pub version: Option<String>,
    pub ipc_protocol: u32,
    pub ipc_endpoint: Option<String>,
    pub secret_location: Option<String>,
    pub has_secret: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountStatus {
    #[default]
    Unknown,
    Guest,
    EmailNotVerified,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CatalogView {
    pub accepted: TunnelCatalog,
    pub pending: Vec<PendingTunnel>,
    pub account_status: AccountStatus,
    pub login_url: Option<String>,
    pub last_problem: Option<Problem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTunnel {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrafficSnapshot {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_tcp: u32,
    pub active_udp: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub problem: Problem,
    pub content: Option<NoticeContent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemReport {
    pub problem: Problem,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeContent {
    pub priority: NoticePriority,
    pub message: String,
    pub resolve_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticePriority {
    Info,
    Critical,
    High,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSnapshot {
    pub revision: u64,
    pub phase: Phase,
    pub service: ServiceInfo,
    pub catalog: CatalogView,
    pub traffic: TrafficSnapshot,
    pub notices: Vec<Notice>,
    pub last_problem: Option<ProblemReport>,
}

impl AppSnapshot {
    pub fn booting() -> Self {
        Self {
            revision: 0,
            phase: Phase::Booting,
            service: ServiceInfo::default(),
            catalog: CatalogView::default(),
            traffic: TrafficSnapshot::default(),
            notices: Vec::new(),
            last_problem: None,
        }
    }

    pub fn revise(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    pub snapshot: AppSnapshot,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            snapshot: AppSnapshot::booting(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretInput(String);

impl SecretInput {
    pub fn new(value: impl Into<String>) -> Result<Self, SecretInputError> {
        let value = value.into();
        (!value.trim().is_empty())
            .then_some(Self(value))
            .ok_or(SecretInputError::Empty)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretInput([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretInputError {
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    BeginClaim,
    PollClaim { code: String },
    ExchangeClaim { code: String },
    ProvisionSecret { secret: SecretInput },
    ResetSecret,
    CreateLoginUrl,
    RefreshCatalog,
    Stop,
}

impl Command {
    pub const fn kind(&self) -> CommandKind {
        match self {
            Self::BeginClaim => CommandKind::BeginClaim,
            Self::PollClaim { .. } => CommandKind::PollClaim,
            Self::ExchangeClaim { .. } => CommandKind::ExchangeClaim,
            Self::ProvisionSecret { .. } => CommandKind::ProvisionSecret,
            Self::ResetSecret => CommandKind::ResetSecret,
            Self::CreateLoginUrl => CommandKind::CreateLoginUrl,
            Self::RefreshCatalog => CommandKind::RefreshCatalog,
            Self::Stop => CommandKind::Stop,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    BeginClaim,
    PollClaim,
    ExchangeClaim,
    ProvisionSecret,
    ResetSecret,
    CreateLoginUrl,
    RefreshCatalog,
    Stop,
}

impl CommandKind {
    pub const ALL: [Self; 8] = [
        Self::BeginClaim,
        Self::PollClaim,
        Self::ExchangeClaim,
        Self::ProvisionSecret,
        Self::ResetSecret,
        Self::CreateLoginUrl,
        Self::RefreshCatalog,
        Self::Stop,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Boot,
    SecretMissing,
    SecretInvalid,
    SecretLoaded,
    SecretPersisted,
    SecretPersistenceFailed,
    EngineStarted {
        agent: AgentIdentity,
    },
    EngineStartFailed {
        problem: Problem,
        retry_at: Deadline,
    },
    RetryElapsed {
        attempt: u32,
    },
    CatalogLoaded {
        catalog: TunnelCatalog,
    },
    CatalogLoadFailed {
        retry_at: Deadline,
    },
    ShutdownComplete,
    ShutdownTimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    BindIpc,
    LoadSecret,
    StartClaim,
    PollClaim,
    ExchangeClaim,
    PersistSecret(SecretInput),
    ResetSecret,
    CreateLoginUrl,
    StartEngine,
    StopEngine,
    FetchCatalog,
    ScheduleRetry(Deadline),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandDecision {
    Accepted {
        state: AppState,
        effects: Vec<Effect>,
    },
    Rejected {
        state: AppState,
        problem: Problem,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventDecision {
    Applied {
        state: AppState,
        effects: Vec<Effect>,
    },
    Rejected {
        state: AppState,
        problem: Problem,
    },
}

pub fn decide_command(mut state: AppState, command: Command) -> CommandDecision {
    let phase = &state.snapshot.phase;
    let effects = match command {
        Command::BeginClaim if matches!(phase, Phase::NeedsSecret { .. }) => {
            vec![Effect::StartClaim]
        }
        Command::PollClaim { .. } if matches!(phase, Phase::NeedsSecret { .. }) => {
            vec![Effect::PollClaim]
        }
        Command::ExchangeClaim { .. } if matches!(phase, Phase::NeedsSecret { .. }) => {
            vec![Effect::ExchangeClaim]
        }
        Command::ProvisionSecret { secret } if matches!(phase, Phase::NeedsSecret { .. }) => {
            set_phase(&mut state, Phase::Starting { attempt: 1 });
            vec![Effect::PersistSecret(secret)]
        }
        Command::ResetSecret
            if matches!(
                phase,
                Phase::NeedsSecret { .. }
                    | Phase::Starting { .. }
                    | Phase::Blocked { .. }
                    | Phase::Online { .. }
            ) =>
        {
            set_phase(&mut state, Phase::Stopping);
            vec![Effect::ResetSecret]
        }
        Command::CreateLoginUrl
            if matches!(phase, Phase::NeedsSecret { .. } | Phase::Online { .. }) =>
        {
            vec![Effect::CreateLoginUrl]
        }
        Command::RefreshCatalog if matches!(phase, Phase::Online { .. }) => {
            vec![Effect::FetchCatalog]
        }
        Command::Stop
            if matches!(
                phase,
                Phase::Booting
                    | Phase::NeedsSecret { .. }
                    | Phase::Starting { .. }
                    | Phase::Blocked { .. }
                    | Phase::Online { .. }
            ) =>
        {
            set_phase(&mut state, Phase::Stopping);
            vec![Effect::StopEngine]
        }
        _ => {
            let problem = match &state.snapshot.phase {
                Phase::Blocked { problem, .. } => problem.clone(),
                _ => Problem::new(ProblemCode::CommandNotAllowed),
            };
            return CommandDecision::Rejected { state, problem };
        }
    };

    CommandDecision::Accepted { state, effects }
}

pub fn reduce(mut state: AppState, event: Event) -> EventDecision {
    let phase = &state.snapshot.phase;
    let effects = match event {
        Event::Boot if matches!(phase, Phase::Booting) => {
            vec![Effect::BindIpc, Effect::LoadSecret]
        }
        Event::SecretMissing if matches!(phase, Phase::Booting | Phase::Starting { .. }) => {
            set_phase(
                &mut state,
                Phase::NeedsSecret {
                    reason: SecretNeed::Missing,
                },
            );
            Vec::new()
        }
        Event::SecretInvalid if matches!(phase, Phase::Booting | Phase::Starting { .. }) => {
            set_phase(
                &mut state,
                Phase::NeedsSecret {
                    reason: SecretNeed::Invalid,
                },
            );
            Vec::new()
        }
        Event::SecretLoaded if matches!(phase, Phase::Booting | Phase::NeedsSecret { .. }) => {
            set_phase(&mut state, Phase::Starting { attempt: 1 });
            vec![Effect::StartEngine]
        }
        Event::SecretPersisted if matches!(phase, Phase::Starting { .. }) => {
            vec![Effect::StartEngine]
        }
        Event::SecretPersistenceFailed if matches!(phase, Phase::Starting { .. }) => {
            state.snapshot.phase = Phase::NeedsSecret {
                reason: SecretNeed::PersistenceFailed,
            };
            state.snapshot.notices.push(Notice {
                problem: Problem::new(ProblemCode::SecretWriteFailed),
                content: None,
            });
            state.snapshot.revise();
            Vec::new()
        }
        Event::EngineStarted { agent } if matches!(phase, Phase::Starting { .. }) => {
            set_phase(&mut state, Phase::Online { agent });
            vec![Effect::FetchCatalog]
        }
        Event::EngineStartFailed { problem, retry_at }
            if matches!(phase, Phase::Starting { .. }) =>
        {
            set_phase(&mut state, Phase::Blocked { problem, retry_at });
            vec![Effect::ScheduleRetry(retry_at)]
        }
        Event::RetryElapsed { attempt } if matches!(phase, Phase::Blocked { .. }) => {
            set_phase(
                &mut state,
                Phase::Starting {
                    attempt: attempt.max(1),
                },
            );
            vec![Effect::StartEngine]
        }
        Event::CatalogLoaded { catalog } if matches!(phase, Phase::Online { .. }) => {
            state.snapshot.catalog.accepted = catalog;
            state.snapshot.catalog.last_problem = None;
            state.snapshot.revise();
            Vec::new()
        }
        Event::CatalogLoadFailed { retry_at } if matches!(phase, Phase::Online { .. }) => {
            state.snapshot.catalog.last_problem =
                Some(Problem::new(ProblemCode::CatalogUnavailable));
            state.snapshot.revise();
            vec![Effect::ScheduleRetry(retry_at)]
        }
        Event::ShutdownComplete if matches!(phase, Phase::Stopping) => {
            set_phase(&mut state, Phase::Stopped);
            Vec::new()
        }
        Event::ShutdownTimedOut if matches!(phase, Phase::Stopping) => {
            state.snapshot.notices.push(Notice {
                problem: Problem::new(ProblemCode::ShutdownTimedOut),
                content: None,
            });
            set_phase(&mut state, Phase::Stopped);
            Vec::new()
        }
        _ => {
            return EventDecision::Rejected {
                state,
                problem: Problem::new(ProblemCode::EventNotApplicable),
            };
        }
    };

    EventDecision::Applied { state, effects }
}

fn set_phase(state: &mut AppState, phase: Phase) {
    if state.snapshot.phase != phase {
        state.snapshot.phase = phase;
        state.snapshot.revise();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phases() -> Vec<Phase> {
        vec![
            Phase::Booting,
            Phase::NeedsSecret {
                reason: SecretNeed::Missing,
            },
            Phase::Starting { attempt: 1 },
            Phase::Blocked {
                problem: Problem::new(ProblemCode::AgentDisabledOverLimit),
                retry_at: Deadline::AtMillis(100),
            },
            Phase::Online {
                agent: AgentIdentity::new("agent-1"),
            },
            Phase::Stopping,
            Phase::Stopped,
        ]
    }

    fn state(phase: Phase) -> AppState {
        AppState {
            snapshot: AppSnapshot {
                phase,
                ..AppSnapshot::booting()
            },
        }
    }

    fn command(kind: CommandKind) -> Command {
        match kind {
            CommandKind::BeginClaim => Command::BeginClaim,
            CommandKind::PollClaim => Command::PollClaim {
                code: "fixture".to_owned(),
            },
            CommandKind::ExchangeClaim => Command::ExchangeClaim {
                code: "fixture".to_owned(),
            },
            CommandKind::ProvisionSecret => Command::ProvisionSecret {
                secret: SecretInput::new("secret").unwrap(),
            },
            CommandKind::ResetSecret => Command::ResetSecret,
            CommandKind::CreateLoginUrl => Command::CreateLoginUrl,
            CommandKind::RefreshCatalog => Command::RefreshCatalog,
            CommandKind::Stop => Command::Stop,
        }
    }

    fn accepted(command: CommandKind, phase: &Phase) -> bool {
        match command {
            CommandKind::BeginClaim
            | CommandKind::PollClaim
            | CommandKind::ExchangeClaim
            | CommandKind::ProvisionSecret => {
                matches!(phase, Phase::NeedsSecret { .. })
            }
            CommandKind::ResetSecret => {
                matches!(
                    phase,
                    Phase::NeedsSecret { .. }
                        | Phase::Starting { .. }
                        | Phase::Blocked { .. }
                        | Phase::Online { .. }
                )
            }
            CommandKind::CreateLoginUrl => {
                matches!(phase, Phase::NeedsSecret { .. } | Phase::Online { .. })
            }
            CommandKind::RefreshCatalog => matches!(phase, Phase::Online { .. }),
            CommandKind::Stop => !matches!(phase, Phase::Stopping | Phase::Stopped),
        }
    }

    #[test]
    fn every_command_and_phase_pair_has_an_explicit_decision() {
        let mut covered = 0;
        let phases = phases();
        for phase in &phases {
            for command_kind in CommandKind::ALL {
                covered += 1;
                let before = state(phase.clone());
                let expected = accepted(command_kind, phase);
                let decision = decide_command(before.clone(), command(command_kind));
                match decision {
                    CommandDecision::Accepted { state, effects } => {
                        assert!(
                            expected,
                            "unexpected acceptance for {command_kind:?} in {phase:?}"
                        );
                        assert!(!effects.is_empty());
                        if matches!(
                            command_kind,
                            CommandKind::ProvisionSecret
                                | CommandKind::ResetSecret
                                | CommandKind::Stop
                        ) {
                            assert!(state.snapshot.revision > before.snapshot.revision);
                        }
                    }
                    CommandDecision::Rejected { state, problem } => {
                        assert!(
                            !expected,
                            "unexpected rejection for {command_kind:?} in {phase:?}"
                        );
                        assert_eq!(state, before);
                        let expected_code = if matches!(phase, Phase::Blocked { .. }) {
                            ProblemCode::AgentDisabledOverLimit
                        } else {
                            ProblemCode::CommandNotAllowed
                        };
                        assert_eq!(problem.code, expected_code);
                    }
                }
            }
        }
        assert_eq!(covered, phases.len() * CommandKind::ALL.len());
    }

    #[test]
    fn provisioning_failure_returns_to_secret_input() {
        let provision = decide_command(
            state(Phase::NeedsSecret {
                reason: SecretNeed::Missing,
            }),
            Command::ProvisionSecret {
                secret: SecretInput::new("secret").unwrap(),
            },
        );
        let CommandDecision::Accepted { state, .. } = provision else {
            panic!("provisioning must be accepted");
        };
        let EventDecision::Applied { state, .. } = reduce(state, Event::SecretPersistenceFailed)
        else {
            panic!("persistence result must apply");
        };
        assert!(matches!(
            state.snapshot.phase,
            Phase::NeedsSecret {
                reason: SecretNeed::PersistenceFailed
            }
        ));
        assert_eq!(
            state.snapshot.notices.last().unwrap().problem.code,
            ProblemCode::SecretWriteFailed
        );
    }

    #[test]
    fn catalog_failure_keeps_last_accepted_catalog() {
        let mut online = state(Phase::Online {
            agent: AgentIdentity::new("agent-1"),
        });
        online.snapshot.catalog.accepted = TunnelCatalog::try_new(7, 100, Vec::new()).unwrap();
        let EventDecision::Applied { state, effects } = reduce(
            online,
            Event::CatalogLoadFailed {
                retry_at: Deadline::AtMillis(200),
            },
        ) else {
            panic!("catalog failure must apply while online");
        };
        assert_eq!(state.snapshot.catalog.accepted.revision(), 7);
        assert_eq!(
            state.snapshot.catalog.last_problem.unwrap().code,
            ProblemCode::CatalogUnavailable
        );
        assert_eq!(
            effects,
            vec![Effect::ScheduleRetry(Deadline::AtMillis(200))]
        );
    }

    #[test]
    fn invalid_events_do_not_mutate_state() {
        let before = state(Phase::Stopped);
        assert_eq!(
            reduce(before.clone(), Event::SecretLoaded),
            EventDecision::Rejected {
                state: before,
                problem: Problem::new(ProblemCode::EventNotApplicable),
            }
        );
    }

    #[test]
    fn retry_elapsed_preserves_the_supervisors_attempt_count() {
        let blocked = state(Phase::Blocked {
            problem: Problem::new(ProblemCode::AgentDisabledOverLimit),
            retry_at: Deadline::AtMillis(100),
        });
        let EventDecision::Applied { state, .. } =
            reduce(blocked, Event::RetryElapsed { attempt: 3 })
        else {
            panic!("retry event must apply while blocked");
        };
        assert_eq!(state.snapshot.phase, Phase::Starting { attempt: 3 });
    }
}
