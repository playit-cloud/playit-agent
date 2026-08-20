#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProblemCode {
    Internal,
    UnsupportedProtocol,
    InvalidRequest,
    AgentDisabledOverLimit,
    InvalidSecret,
    SecretPinned,
    ProvisioningUnavailable,
    SecretWriteFailed,
    StartupFailed,
    EngineUnavailable,
    CatalogUnavailable,
    CatalogInvalid,
    TunnelDisabled,
    RemoteNotice,
    ShutdownTimedOut,
    CommandNotAllowed,
    EventNotApplicable,
}

impl ProblemCode {
    pub const ALL: [Self; 17] = [
        Self::Internal,
        Self::UnsupportedProtocol,
        Self::InvalidRequest,
        Self::AgentDisabledOverLimit,
        Self::InvalidSecret,
        Self::SecretPinned,
        Self::ProvisioningUnavailable,
        Self::SecretWriteFailed,
        Self::StartupFailed,
        Self::EngineUnavailable,
        Self::CatalogUnavailable,
        Self::CatalogInvalid,
        Self::TunnelDisabled,
        Self::RemoteNotice,
        Self::ShutdownTimedOut,
        Self::CommandNotAllowed,
        Self::EventNotApplicable,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::InvalidRequest => "invalid_request",
            Self::AgentDisabledOverLimit => "agent_disabled_over_limit",
            Self::InvalidSecret => "invalid_secret",
            Self::SecretPinned => "secret_pinned",
            Self::ProvisioningUnavailable => "provisioning_unavailable",
            Self::SecretWriteFailed => "secret_write_failed",
            Self::StartupFailed => "startup_failed",
            Self::EngineUnavailable => "engine_unavailable",
            Self::CatalogUnavailable => "catalog_unavailable",
            Self::CatalogInvalid => "catalog_invalid",
            Self::TunnelDisabled => "tunnel_disabled",
            Self::RemoteNotice => "remote_notice",
            Self::ShutdownTimedOut => "shutdown_timed_out",
            Self::CommandNotAllowed => "command_not_allowed",
            Self::EventNotApplicable => "event_not_applicable",
        }
    }

    pub const fn metadata(self) -> ProblemMetadata {
        match self {
            Self::AgentDisabledOverLimit => ProblemMetadata {
                severity: Severity::Error,
                retry: RetryPolicy::AfterBackoff,
                action: UserAction::ReviewAccount,
            },
            Self::InvalidSecret | Self::SecretWriteFailed => ProblemMetadata {
                severity: Severity::Error,
                retry: RetryPolicy::UserInitiated,
                action: UserAction::ProvisionSecret,
            },
            Self::SecretPinned => ProblemMetadata {
                severity: Severity::Error,
                retry: RetryPolicy::Never,
                action: UserAction::RestartWithConfiguration,
            },
            Self::UnsupportedProtocol
            | Self::InvalidRequest
            | Self::CommandNotAllowed
            | Self::EventNotApplicable => ProblemMetadata {
                severity: Severity::Warning,
                retry: RetryPolicy::Never,
                action: UserAction::None,
            },
            Self::ProvisioningUnavailable
            | Self::CatalogUnavailable
            | Self::EngineUnavailable
            | Self::StartupFailed => ProblemMetadata {
                severity: Severity::Error,
                retry: RetryPolicy::AfterBackoff,
                action: UserAction::Retry,
            },
            Self::CatalogInvalid | Self::TunnelDisabled | Self::RemoteNotice => ProblemMetadata {
                severity: Severity::Warning,
                retry: RetryPolicy::UserInitiated,
                action: UserAction::ReviewConfiguration,
            },
            Self::ShutdownTimedOut => ProblemMetadata {
                severity: Severity::Error,
                retry: RetryPolicy::Never,
                action: UserAction::RestartService,
            },
            Self::Internal => ProblemMetadata {
                severity: Severity::Error,
                retry: RetryPolicy::UserInitiated,
                action: UserAction::RestartService,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProblemMetadata {
    pub severity: Severity,
    pub retry: RetryPolicy,
    pub action: UserAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryPolicy {
    Never,
    Immediate,
    AfterBackoff,
    UserInitiated,
}

impl RetryPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Immediate => "immediate",
            Self::AfterBackoff => "after_backoff",
            Self::UserInitiated => "user_initiated",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserAction {
    None,
    ProvisionSecret,
    ReviewAccount,
    ReviewConfiguration,
    Retry,
    RestartService,
    RestartWithConfiguration,
}

impl UserAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ProvisionSecret => "provision_secret",
            Self::ReviewAccount => "review_account",
            Self::ReviewConfiguration => "review_configuration",
            Self::Retry => "retry",
            Self::RestartService => "restart_service",
            Self::RestartWithConfiguration => "restart_with_configuration",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProblemSubject {
    pub kind: SubjectKind,
    pub id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectKind {
    Tunnel,
    ConfigurationField,
    ChildService,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub code: ProblemCode,
    pub metadata: ProblemMetadata,
    pub subject: Option<ProblemSubject>,
}

impl Problem {
    pub const fn new(code: ProblemCode) -> Self {
        Self {
            code,
            metadata: code.metadata(),
            subject: None,
        }
    }

    pub const fn with_subject(mut self, subject: ProblemSubject) -> Self {
        self.subject = Some(subject);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_codes_are_unique() {
        let mut values: Vec<_> = ProblemCode::ALL
            .into_iter()
            .map(ProblemCode::as_str)
            .collect();
        let count = values.len();
        values.sort_unstable();
        values.dedup();
        assert_eq!(values.len(), count);
    }

    #[test]
    fn metadata_encodes_actions_without_copy() {
        assert_eq!(
            ProblemCode::AgentDisabledOverLimit.metadata(),
            ProblemMetadata {
                severity: Severity::Error,
                retry: RetryPolicy::AfterBackoff,
                action: UserAction::ReviewAccount,
            }
        );
        assert_eq!(
            Problem::new(ProblemCode::InvalidSecret).metadata.action,
            UserAction::ProvisionSecret
        );
    }
}
