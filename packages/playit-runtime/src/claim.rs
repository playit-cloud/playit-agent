use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use playit_model::{Problem, ProblemCode};
use rand::Rng;

use crate::GeneratedClientGateway;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimMode {
    Assignable,
    SelfManaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimProgress {
    WaitingForVisit,
    WaitingForApproval,
    Approved,
    Rejected,
}

#[derive(Clone, PartialEq, Eq)]
pub enum ClaimExchange {
    Pending(String),
    Accepted(String),
}

impl fmt::Debug for ClaimExchange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending(status) => formatter.debug_tuple("Pending").field(status).finish(),
            Self::Accepted(_) => formatter.write_str("Accepted([REDACTED])"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimSession {
    pub code: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimFailure {
    pub problem: Problem,
    pub detail: String,
}

pub struct ClaimService {
    gateway: Arc<dyn ClaimGateway>,
    version: String,
    timeout: Duration,
}

impl ClaimService {
    pub fn new(api_base: String, version: String) -> Self {
        Self {
            gateway: Arc::new(GeneratedClientGateway::without_secret(api_base)),
            version,
            timeout: Duration::from_secs(10),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_gateway(gateway: Arc<dyn ClaimGateway>, version: String) -> Self {
        Self {
            gateway,
            version,
            timeout: Duration::from_secs(1),
        }
    }

    pub fn begin() -> ClaimSession {
        let mut buffer = [0u8; 5];
        rand::rng().fill(&mut buffer);
        let code = hex::encode(buffer);
        ClaimSession {
            url: format!("https://playit.gg/claim/{code}"),
            code,
        }
    }

    pub async fn progress(
        &self,
        code: &str,
        mode: ClaimMode,
    ) -> Result<ClaimProgress, ClaimFailure> {
        tokio::time::timeout(
            self.timeout,
            self.gateway.progress(code, mode, &self.version),
        )
        .await
        .unwrap_or_else(|_| Err(claim_timeout()))
    }

    pub async fn exchange(&self, code: &str) -> Result<ClaimExchange, ClaimFailure> {
        tokio::time::timeout(self.timeout, self.gateway.exchange(code))
            .await
            .unwrap_or_else(|_| Err(claim_timeout()))
    }
}

fn claim_timeout() -> ClaimFailure {
    ClaimFailure {
        problem: Problem::new(ProblemCode::ProvisioningUnavailable),
        detail: "claim request timed out".to_owned(),
    }
}

#[async_trait]
pub(crate) trait ClaimGateway: Send + Sync {
    async fn progress(
        &self,
        code: &str,
        mode: ClaimMode,
        version: &str,
    ) -> Result<ClaimProgress, ClaimFailure>;

    async fn exchange(&self, code: &str) -> Result<ClaimExchange, ClaimFailure>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_claim_session_has_stable_url_shape() {
        let session = ClaimService::begin();
        assert_eq!(session.code.len(), 10);
        assert!(hex::decode(&session.code).is_ok());
        assert_eq!(
            session.url,
            format!("https://playit.gg/claim/{}", session.code)
        );
    }

    #[test]
    fn accepted_claim_debug_output_redacts_the_secret() {
        let exchange = ClaimExchange::Accepted("sensitive-agent-key".to_owned());
        let output = format!("{exchange:?}");
        assert_eq!(output, "Accepted([REDACTED])");
        assert!(!output.contains("sensitive-agent-key"));
    }
}
