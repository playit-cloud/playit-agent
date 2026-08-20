use std::future::Future;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstalledServiceStopPolicy {
    pub graceful_wait: Duration,
    pub final_wait: Duration,
}

impl Default for InstalledServiceStopPolicy {
    fn default() -> Self {
        Self {
            graceful_wait: Duration::from_secs(1),
            final_wait: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GracefulStopAttempt {
    Accepted,
    Rejected(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstalledServiceStopOutcome {
    AlreadyStopped,
    Stopped,
    StillRunning,
    StatusUnknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledServiceStopReport {
    pub outcome: InstalledServiceStopOutcome,
    pub graceful: Option<GracefulStopAttempt>,
    pub fallback: Option<Result<(), String>>,
    pub initial_status_error: Option<String>,
}

pub async fn stop_installed_service_with_fallback<
    IsRunning,
    IsRunningFuture,
    GracefulStop,
    GracefulStopFuture,
    DirectStop,
    DirectStopFuture,
>(
    policy: InstalledServiceStopPolicy,
    mut is_running: IsRunning,
    graceful_stop: GracefulStop,
    direct_stop: DirectStop,
) -> InstalledServiceStopReport
where
    IsRunning: FnMut() -> IsRunningFuture,
    IsRunningFuture: Future<Output = Result<bool, String>>,
    GracefulStop: FnOnce() -> GracefulStopFuture,
    GracefulStopFuture: Future<Output = GracefulStopAttempt>,
    DirectStop: FnOnce() -> DirectStopFuture,
    DirectStopFuture: Future<Output = Result<(), String>>,
{
    let initial_status_error = match is_running().await {
        Ok(false) => {
            return InstalledServiceStopReport {
                outcome: InstalledServiceStopOutcome::AlreadyStopped,
                graceful: None,
                fallback: None,
                initial_status_error: None,
            };
        }
        Ok(true) => None,
        Err(error) => Some(error),
    };

    let graceful = graceful_stop().await;
    let fallback = if graceful == GracefulStopAttempt::Accepted {
        tokio::time::sleep(policy.graceful_wait).await;
        None
    } else {
        Some(direct_stop().await)
    };

    tokio::time::sleep(policy.final_wait).await;
    let outcome = match is_running().await {
        Ok(false) => InstalledServiceStopOutcome::Stopped,
        Ok(true) => InstalledServiceStopOutcome::StillRunning,
        Err(error) => InstalledServiceStopOutcome::StatusUnknown(error),
    };

    InstalledServiceStopReport {
        outcome,
        graceful: Some(graceful),
        fallback,
        initial_status_error,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn no_wait() -> InstalledServiceStopPolicy {
        InstalledServiceStopPolicy {
            graceful_wait: Duration::ZERO,
            final_wait: Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn accepted_graceful_stop_does_not_invoke_fallback() {
        let checks = Cell::new(0);
        let fallback_called = Cell::new(false);
        let report = stop_installed_service_with_fallback(
            no_wait(),
            || {
                let check = checks.get();
                checks.set(check + 1);
                async move { Ok(check == 0) }
            },
            || async { GracefulStopAttempt::Accepted },
            || {
                fallback_called.set(true);
                async { Ok(()) }
            },
        )
        .await;

        assert_eq!(report.outcome, InstalledServiceStopOutcome::Stopped);
        assert_eq!(report.graceful, Some(GracefulStopAttempt::Accepted));
        assert_eq!(report.fallback, None);
        assert!(!fallback_called.get());
    }

    #[tokio::test]
    async fn rejected_graceful_stop_invokes_fallback_once() {
        let checks = Cell::new(0);
        let fallback_calls = Cell::new(0);
        let report = stop_installed_service_with_fallback(
            no_wait(),
            || {
                let check = checks.get();
                checks.set(check + 1);
                async move { Ok(check == 0) }
            },
            || async { GracefulStopAttempt::Rejected("rejected".to_owned()) },
            || {
                fallback_calls.set(fallback_calls.get() + 1);
                async { Ok(()) }
            },
        )
        .await;

        assert_eq!(report.outcome, InstalledServiceStopOutcome::Stopped);
        assert_eq!(fallback_calls.get(), 1);
        assert_eq!(report.fallback, Some(Ok(())));
    }

    #[tokio::test]
    async fn stopped_service_skips_both_stop_mechanisms() {
        let report = stop_installed_service_with_fallback(
            no_wait(),
            || async { Ok(false) },
            || async { panic!("graceful stop must not run") },
            || async { panic!("fallback stop must not run") },
        )
        .await;

        assert_eq!(report.outcome, InstalledServiceStopOutcome::AlreadyStopped);
        assert_eq!(report.graceful, None);
        assert_eq!(report.fallback, None);
    }
}
