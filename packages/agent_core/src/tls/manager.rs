use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::{BodyExt, Empty, Limited};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, CertificateIdentifier, ChallengeType,
    Identifier, LetsEncrypt, NewAccount, NewOrder, OrderStatus, Problem, RetryPolicy,
};
use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, Der, PrivateKeyDer, pem::PemObject};
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use x509_parser::extensions::ParsedExtension;

use super::store::{CertStore, StoredCert, is_valid_domain};
use crate::utils::now_milli;

const SECOND_MS: u64 = 1000;
const MINUTE_MS: u64 = 60 * SECOND_MS;
const HOUR_MS: u64 = 60 * MINUTE_MS;
const DAY_MS: u64 = 24 * HOUR_MS;

/// Without renewal information from the ACME server, certificates are renewed once they have
/// fewer than this many days left. Let's Encrypt issues 90 day certificates and recommends
/// renewing at two thirds of their lifetime.
const RENEW_BEFORE_MS: u64 = 30 * DAY_MS;
/// How often the worker re-checks expiry when nothing else wakes it.
const IDLE_CHECK_MS: u64 = 6 * HOUR_MS;
const RETRY_DELAYS_SECS: [u64; 5] = [60, 5 * 60, 15 * 60, 60 * 60, 3 * 60 * 60];
/// The ACME client has no request timeout of its own, so a stalled connection would
/// otherwise hold up every other domain. The reachability check before ordering has its own
/// bounds and doesn't count against this.
const ISSUE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Bounds on the wait after Let's Encrypt reports a rate limit. Its "retry after" time is
/// used when present; without one the wait is at least an hour, since the limits are counted
/// over hours or days.
const RATE_LIMIT_MIN_WAIT_MS: u64 = 5 * MINUTE_MS;
const RATE_LIMIT_MAX_WAIT_MS: u64 = 7 * DAY_MS;
const RATE_LIMIT_DEFAULT_WAIT_MS: u64 = HOUR_MS;
/// A removed domain's failures are remembered this long, so disabling and re-enabling a
/// tunnel doesn't skip the backoff.
const FORGET_REMOVED_MS: u64 = DAY_MS;
/// ACME Renewal Information (RFC 9773) is fetched again after the server's Retry-After,
/// within these bounds, or after the error delay when fetching it fails.
const ARI_TIMEOUT: Duration = Duration::from_secs(30);
const ARI_MIN_RECHECK_MS: u64 = HOUR_MS;
const ARI_MAX_RECHECK_MS: u64 = DAY_MS;
const ARI_ERROR_RECHECK_MS: u64 = 6 * HOUR_MS;
/// Asking Let's Encrypt to validate a domain it can't reach counts against its failed
/// validation limit, so the agent first fetches a test token through the tunnel itself.
const SELF_CHECK_ATTEMPTS: u32 = 12;
const SELF_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const SKIP_SELF_CHECK_ENV: &str = "PLAYIT_ACME_SKIP_SELF_CHECK";

pub const ACME_CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

/// Where a tunnel's certificate is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertStatus {
    /// Loading a stored certificate, or waiting for the agent to connect before requesting one.
    Preparing,
    /// Fetching a test token through the tunnel to make sure Let's Encrypt can reach it.
    CheckingReachability,
    /// Creating the certificate order with Let's Encrypt.
    Requesting,
    /// Let's Encrypt is fetching the HTTP challenge through the tunnel.
    Validating,
    /// The domain is validated and the certificate is being issued.
    Issuing,
    /// A certificate is installed. `renewing` is set while a replacement is being requested.
    Ready { expires_at: u64, renewing: bool },
    /// No certificate could be obtained. Another attempt starts at `retry_at`.
    Failed { message: String, retry_at: u64 },
}

impl CertStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, CertStatus::Ready { .. })
    }
}

/// Obtains and renews Let's Encrypt certificates for HTTPS tunnels that terminate TLS in
/// the agent, and answers the HTTP-01 challenges Let's Encrypt sends through the tunnel.
#[derive(Clone)]
pub struct CertManager {
    inner: Arc<Inner>,
}

struct Inner {
    store: CertStore,
    directory_url: String,
    /// A PEM root certificate to trust for the ACME server, for private or test servers.
    acme_root: Option<std::path::PathBuf>,
    self_check: SelfCheck,
    issue_timeout: Duration,
    domains: Mutex<HashMap<String, DomainState>>,
    /// Backoff of domains that were removed recently. Locked after `domains`.
    removed: Mutex<HashMap<String, RemovedDomain>>,
    challenges: Mutex<HashMap<String, String>>,
    /// Set while the agent is connected, since challenges can only be answered then.
    active: AtomicBool,
    /// Set once the ACME server turns out not to offer renewal information.
    ari_unsupported: AtomicBool,
    wake: Notify,
}

/// Fetching a test token through the tunnel before ordering a certificate.
struct SelfCheck {
    enabled: bool,
    /// Where to send the request instead of the domain itself; used by tests.
    host: Option<String>,
    attempts: u32,
    interval: Duration,
}

impl SelfCheck {
    fn new(enabled: bool) -> Self {
        SelfCheck {
            enabled,
            host: None,
            attempts: SELF_CHECK_ATTEMPTS,
            interval: SELF_CHECK_INTERVAL,
        }
    }
}

struct DomainState {
    status: CertStatus,
    acceptor: Option<TlsAcceptor>,
    expires_at: Option<u64>,
    /// Identifies the installed certificate to the ACME server's renewal information.
    cert_id: Option<CertificateIdentifier<'static>>,
    /// The suggested renewal window from the ACME server, and the time picked within it.
    ari_window: Option<(u64, u64)>,
    ari_renew_at: Option<u64>,
    ari_check_at: u64,
    loaded_from_disk: bool,
    failures: u32,
    next_attempt: u64,
}

struct RemovedDomain {
    failures: u32,
    next_attempt: u64,
    status: CertStatus,
    removed_at: u64,
}

impl DomainState {
    fn new() -> Self {
        DomainState {
            status: CertStatus::Preparing,
            acceptor: None,
            expires_at: None,
            cert_id: None,
            ari_window: None,
            ari_renew_at: None,
            ari_check_at: 0,
            loaded_from_disk: false,
            failures: 0,
            next_attempt: 0,
        }
    }

    fn install(&mut self, cert: LoadedCert) {
        self.acceptor = Some(cert.acceptor);
        self.expires_at = Some(cert.expires_at);
        self.cert_id = cert.cert_id;
        self.ari_window = None;
        self.ari_renew_at = None;
        self.ari_check_at = 0;
        self.status = CertStatus::Ready {
            expires_at: cert.expires_at,
            renewing: false,
        };
    }

    fn has_valid_certificate(&self, now: u64) -> bool {
        self.acceptor.is_some() && self.expires_at.is_some_and(|expires_at| expires_at > now)
    }

    /// When the installed certificate should be replaced: the time picked from the server's
    /// renewal window, or 30 days before expiry when the server gave none.
    fn renew_at(&self) -> Option<u64> {
        let expires_at = self.expires_at?;
        Some(match self.ari_renew_at {
            // A window that runs up to the expiry would leave no time to retry.
            Some(at) => at.min(expires_at.saturating_sub(DAY_MS)),
            None => expires_at.saturating_sub(RENEW_BEFORE_MS),
        })
    }

    fn needs_certificate(&self, now: u64) -> bool {
        self.renew_at().is_none_or(|renew_at| now >= renew_at)
    }
}

/// A parsed certificate ready to serve.
struct LoadedCert {
    acceptor: TlsAcceptor,
    expires_at: u64,
    cert_id: Option<CertificateIdentifier<'static>>,
}

/// Why an attempt to obtain a certificate failed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failure {
    message: String,
    kind: FailureKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    Other,
    /// The ACME server no longer accepts the saved account.
    AccountInvalid,
    /// Let's Encrypt rate limited the request; `retry_at` is when it said to retry.
    RateLimited {
        retry_at: Option<u64>,
    },
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure {
            message,
            kind: FailureKind::Other,
        }
    }
}

impl Failure {
    fn acme(context: &str, error: instant_acme::Error) -> Self {
        let kind = match &error {
            instant_acme::Error::Api(problem) => classify_problem(problem),
            _ => FailureKind::Other,
        };
        let message = match (&kind, &error) {
            (FailureKind::RateLimited { .. }, instant_acme::Error::Api(problem)) => format!(
                "rate limited by Let's Encrypt: {}",
                problem.detail.as_deref().unwrap_or("too many requests")
            ),
            _ => format!("{context}: {error}"),
        };
        Failure { message, kind }
    }
}

fn classify_problem(problem: &Problem) -> FailureKind {
    let problem_type = problem.r#type.as_deref().unwrap_or_default();
    let problem_type = problem_type
        .strip_prefix("urn:ietf:params:acme:error:")
        .unwrap_or(problem_type);
    let detail = problem.detail.as_deref().unwrap_or_default();

    match problem_type {
        "rateLimited" => FailureKind::RateLimited {
            retry_at: retry_after_from_detail(detail),
        },
        "accountDoesNotExist" => FailureKind::AccountInvalid,
        // Let's Encrypt reports a deactivated or revoked account as "unauthorized", which is
        // also used for unrelated refusals, so the detail has to be about the account.
        "unauthorized" => {
            let detail = detail.to_ascii_lowercase();
            let about_account = detail.contains("account")
                && [
                    "deactivated",
                    "revoked",
                    "not valid",
                    "does not exist",
                    "not found",
                ]
                .iter()
                .any(|phrase| detail.contains(phrase));
            if about_account {
                FailureKind::AccountInvalid
            } else {
                FailureKind::Other
            }
        }
        _ => FailureKind::Other,
    }
}

/// instant-acme doesn't expose the Retry-After header of an error response, but Let's
/// Encrypt repeats it in the problem detail as "retry after 2025-01-25 21:57:49 UTC".
fn retry_after_from_detail(detail: &str) -> Option<u64> {
    const MARKER: &str = "retry after ";
    let start = detail.to_ascii_lowercase().find(MARKER)? + MARKER.len();
    let timestamp = detail.get(start..start + 19)?;
    let parsed = chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S").ok()?;
    u64::try_from(parsed.and_utc().timestamp_millis()).ok()
}

/// How long to wait before the next attempt after `failures` consecutive failures.
fn retry_delay_ms(failures: u32, kind: FailureKind, now: u64) -> u64 {
    let index = (failures.max(1) as usize - 1).min(RETRY_DELAYS_SECS.len() - 1);
    let regular = RETRY_DELAYS_SECS[index] * SECOND_MS;
    match kind {
        FailureKind::RateLimited {
            retry_at: Some(retry_at),
        } => retry_at
            .saturating_sub(now)
            .clamp(RATE_LIMIT_MIN_WAIT_MS, RATE_LIMIT_MAX_WAIT_MS),
        FailureKind::RateLimited { retry_at: None } => regular.max(RATE_LIMIT_DEFAULT_WAIT_MS),
        FailureKind::Other | FailureKind::AccountInvalid => regular,
    }
}

/// The ACME account used by the worker, loaded from disk or registered on first use.
#[derive(Default)]
struct AcmeAccount {
    account: Option<Account>,
    /// The saved credentials were rejected; register a new account instead of loading them.
    replace_saved: bool,
}

impl AcmeAccount {
    /// Returns true when the failure means the account itself is no longer usable, in which
    /// case the next use registers a new account. Other failures keep the credentials.
    fn note_failure(&mut self, kind: FailureKind) -> bool {
        if kind != FailureKind::AccountInvalid {
            return false;
        }
        self.account = None;
        self.replace_saved = true;
        true
    }
}

/// What the ACME server said about when to renew a certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RenewalInfo {
    Window {
        start: u64,
        end: u64,
        recheck_ms: u64,
    },
    Unsupported,
    Failed(String),
}

impl CertManager {
    pub fn new(store: CertStore) -> Self {
        let directory_url = std::env::var("PLAYIT_ACME_DIRECTORY")
            .ok()
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| LetsEncrypt::Production.url().to_string());
        let acme_root = std::env::var_os("PLAYIT_ACME_ROOT_CERT")
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from);
        let skip_self_check = env_flag(std::env::var(SKIP_SELF_CHECK_ENV).ok().as_deref());

        Self::with_acme_server(
            store,
            directory_url,
            acme_root,
            SelfCheck::new(!skip_self_check),
        )
    }

    fn with_acme_server(
        store: CertStore,
        directory_url: String,
        acme_root: Option<std::path::PathBuf>,
        self_check: SelfCheck,
    ) -> Self {
        if !self_check.enabled {
            tracing::info!(
                "skipping the reachability check before requesting TLS certificates ({SKIP_SELF_CHECK_ENV})"
            );
        }
        CertManager {
            inner: Arc::new(Inner {
                store,
                directory_url,
                acme_root,
                self_check,
                issue_timeout: ISSUE_TIMEOUT,
                domains: Mutex::new(HashMap::new()),
                removed: Mutex::new(HashMap::new()),
                challenges: Mutex::new(HashMap::new()),
                active: AtomicBool::new(false),
                ari_unsupported: AtomicBool::new(false),
                wake: Notify::new(),
            }),
        }
    }

    pub fn store(&self) -> &CertStore {
        &self.inner.store
    }

    /// Sets the domains that need certificates. Certificates for removed domains stay on
    /// disk so turning the option back on doesn't request a new one, and their failures are
    /// remembered for a day so it doesn't skip the backoff either.
    pub fn set_domains<I: IntoIterator<Item = String>>(&self, domains: I) {
        let wanted: HashSet<String> = domains
            .into_iter()
            .map(|d| d.to_ascii_lowercase())
            .collect();
        let now = now_milli();
        let mut changed = false;
        {
            let mut current = self.inner.domains.lock().unwrap();
            let mut removed = self.inner.removed.lock().unwrap();
            removed.retain(|_, entry| now.saturating_sub(entry.removed_at) < FORGET_REMOVED_MS);

            current.retain(|domain, state| {
                let keep = wanted.contains(domain);
                if !keep {
                    changed = true;
                    if state.failures > 0 {
                        removed.insert(
                            domain.clone(),
                            RemovedDomain {
                                failures: state.failures,
                                next_attempt: state.next_attempt,
                                status: state.status.clone(),
                                removed_at: now,
                            },
                        );
                    }
                }
                keep
            });

            for domain in wanted {
                let Entry::Vacant(entry) = current.entry(domain) else {
                    continue;
                };
                changed = true;
                let mut state = DomainState::new();
                if let Some(previous) = removed.remove(entry.key()) {
                    state.failures = previous.failures;
                    state.next_attempt = previous.next_attempt;
                    if let CertStatus::Failed { retry_at, .. } = &previous.status
                        && *retry_at > now
                    {
                        state.status = previous.status;
                    }
                }
                entry.insert(state);
            }
        }
        if changed {
            self.inner.wake.notify_one();
        }
    }

    /// Marks whether the agent is connected and can answer challenges through its tunnels.
    pub fn set_active(&self, active: bool) {
        if self.inner.active.swap(active, Ordering::AcqRel) != active {
            self.inner.wake.notify_one();
        }
    }

    pub fn status(&self, domain: &str) -> Option<CertStatus> {
        let now = now_milli();
        let domains = self.inner.domains.lock().unwrap();
        let state = domains.get(&domain.to_ascii_lowercase())?;
        Some(match &state.status {
            CertStatus::Ready { expires_at, .. } if *expires_at <= now => CertStatus::Failed {
                message: "the certificate expired and could not be renewed".to_string(),
                retry_at: state.next_attempt.max(now),
            },
            status => status.clone(),
        })
    }

    /// The acceptor for a domain's certificate, while the certificate is still valid.
    pub fn acceptor(&self, domain: &str) -> Option<TlsAcceptor> {
        let now = now_milli();
        let domains = self.inner.domains.lock().unwrap();
        domains
            .get(&domain.to_ascii_lowercase())
            .filter(|state| state.has_valid_certificate(now))
            .and_then(|state| state.acceptor.clone())
    }

    /// The key authorization for a pending HTTP-01 challenge token.
    pub fn challenge_response(&self, token: &str) -> Option<String> {
        self.inner.challenges.lock().unwrap().get(token).cloned()
    }

    /// Loads stored certificates and requests new ones until `cancel` fires.
    pub async fn run(self, cancel: CancellationToken) {
        let mut account = AcmeAccount::default();

        loop {
            self.load_stored().await;

            let now = now_milli();
            if let Some(domain) = self.next_domain_to_issue(now) {
                let attempt = self.issue(&domain, &mut account);
                tokio::pin!(attempt);
                let result = loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break None,
                        result = &mut attempt => break Some(result),
                        // Domains added during a long attempt still get their stored
                        // certificates right away.
                        _ = self.inner.wake.notified() => self.load_stored().await,
                    }
                };
                let Some(result) = result else { break };
                self.finish_attempt(&domain, result);
                continue;
            }

            if let Some((domain, cert_id)) = self.next_renewal_info_check(now) {
                let info = tokio::select! {
                    _ = cancel.cancelled() => break,
                    info = self.fetch_renewal_info(&mut account, &cert_id) => info,
                };
                self.apply_renewal_info(&domain, &cert_id, info, now_milli());
                continue;
            }

            let sleep_ms = self.next_wake_in(now);
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = self.inner.wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {}
            }
        }

        self.inner.challenges.lock().unwrap().clear();
    }

    async fn load_stored(&self) {
        let pending: Vec<String> = {
            let domains = self.inner.domains.lock().unwrap();
            domains
                .iter()
                .filter(|(_, state)| !state.loaded_from_disk)
                .map(|(domain, _)| domain.clone())
                .collect()
        };

        for domain in pending {
            let loaded = match self.inner.store.load_cert(&domain).await {
                Some(stored) => match load_certificate(&stored) {
                    Ok(loaded) => Some(loaded),
                    Err(error) => {
                        tracing::warn!(%domain, %error, "stored certificate is unusable; requesting a new one");
                        None
                    }
                },
                None => None,
            };

            let mut domains = self.inner.domains.lock().unwrap();
            let Some(state) = domains.get_mut(&domain) else {
                continue;
            };
            state.loaded_from_disk = true;
            if let Some(cert) = loaded
                && cert.expires_at > now_milli()
            {
                tracing::info!(%domain, expires_at = cert.expires_at, "loaded stored TLS certificate");
                state.install(cert);
            }
        }
    }

    fn next_domain_to_issue(&self, now: u64) -> Option<String> {
        if !self.inner.active.load(Ordering::Acquire) {
            return None;
        }
        let domains = self.inner.domains.lock().unwrap();
        domains
            .iter()
            .filter(|(_, state)| {
                state.loaded_from_disk && state.needs_certificate(now) && state.next_attempt <= now
            })
            .min_by_key(|(_, state)| state.next_attempt)
            .map(|(domain, _)| domain.clone())
    }

    fn ari_enabled(&self) -> bool {
        self.inner.active.load(Ordering::Acquire)
            && !self.inner.ari_unsupported.load(Ordering::Acquire)
    }

    fn next_renewal_info_check(
        &self,
        now: u64,
    ) -> Option<(String, CertificateIdentifier<'static>)> {
        if !self.ari_enabled() {
            return None;
        }
        let domains = self.inner.domains.lock().unwrap();
        domains
            .iter()
            .filter(|(_, state)| state.ari_check_at <= now && state.has_valid_certificate(now))
            .filter_map(|(domain, state)| {
                Some((domain, state.cert_id.clone()?, state.ari_check_at))
            })
            .min_by_key(|(_, _, check_at)| *check_at)
            .map(|(domain, cert_id, _)| (domain.clone(), cert_id))
    }

    fn next_wake_in(&self, now: u64) -> u64 {
        // Certificates are only requested while connected, and connecting wakes the worker.
        if !self.inner.active.load(Ordering::Acquire) {
            return IDLE_CHECK_MS;
        }
        let ari = self.ari_enabled();
        let domains = self.inner.domains.lock().unwrap();
        domains
            .values()
            .flat_map(|state| {
                let issue_at = if state.needs_certificate(now) {
                    state.next_attempt
                } else {
                    state.renew_at().unwrap_or(now)
                };
                let ari_at = (ari && state.cert_id.is_some() && state.has_valid_certificate(now))
                    .then_some(state.ari_check_at);
                [Some(issue_at), ari_at]
            })
            .flatten()
            .map(|at| at.saturating_sub(now))
            .min()
            .unwrap_or(IDLE_CHECK_MS)
            .clamp(SECOND_MS, IDLE_CHECK_MS)
    }

    fn set_progress(&self, domain: &str, status: CertStatus) {
        let now = now_milli();
        let mut domains = self.inner.domains.lock().unwrap();
        let Some(state) = domains.get_mut(domain) else {
            return;
        };
        // A working certificate keeps serving while its replacement is requested.
        state.status = match (&state.status, state.has_valid_certificate(now)) {
            (CertStatus::Ready { expires_at, .. }, true) => CertStatus::Ready {
                expires_at: *expires_at,
                renewing: true,
            },
            _ => status,
        };
    }

    fn finish_attempt(&self, domain: &str, result: Result<LoadedCert, Failure>) {
        let now = now_milli();
        let mut domains = self.inner.domains.lock().unwrap();
        let Some(state) = domains.get_mut(domain) else {
            // The domain was removed during the attempt. A failure still counts toward its
            // backoff so re-adding it right away can't skip the wait.
            if let Err(Failure { kind, .. }) = result {
                let mut removed = self.inner.removed.lock().unwrap();
                let previous = removed.remove(domain);
                let failures = previous.as_ref().map_or(0, |p| p.failures) + 1;
                let retry_at = now + retry_delay_ms(failures, kind, now);
                removed.insert(
                    domain.to_string(),
                    RemovedDomain {
                        failures,
                        next_attempt: retry_at,
                        status: CertStatus::Failed {
                            message: "the last certificate request failed".to_string(),
                            retry_at,
                        },
                        removed_at: now,
                    },
                );
            }
            return;
        };

        match result {
            Ok(cert) => {
                tracing::info!(%domain, expires_at = cert.expires_at, "obtained TLS certificate from Let's Encrypt");
                state.install(cert);
                state.failures = 0;
                state.next_attempt = 0;
            }
            Err(Failure { message, kind }) => {
                state.failures += 1;
                let delay = retry_delay_ms(state.failures, kind, now);
                let retry_at = now + delay;
                state.next_attempt = retry_at;
                tracing::error!(%domain, %message, retry_in_secs = delay / SECOND_MS, "failed to obtain TLS certificate");

                state.status = match state.expires_at {
                    Some(expires_at) if state.has_valid_certificate(now) => CertStatus::Ready {
                        expires_at,
                        renewing: false,
                    },
                    _ => {
                        state.acceptor = None;
                        CertStatus::Failed { message, retry_at }
                    }
                };
            }
        }
    }

    fn account_builder(&self) -> Result<instant_acme::AccountBuilder, Failure> {
        match &self.inner.acme_root {
            Some(root) => Account::builder_with_root(root),
            None => Account::builder(),
        }
        .map_err(|e| Failure::acme("could not set up the ACME client", e))
    }

    async fn has_saved_account(&self) -> bool {
        self.inner
            .store
            .load_account()
            .await
            .and_then(|saved| serde_json::from_str::<SavedAccount>(&saved).ok())
            .is_some_and(|saved| saved.directory == self.inner.directory_url)
    }

    async fn account(&self, acme: &mut AcmeAccount) -> Result<Account, Failure> {
        if let Some(account) = &acme.account {
            return Ok(account.clone());
        }

        let directory = &self.inner.directory_url;
        if !acme.replace_saved
            && let Some(saved) = self.inner.store.load_account().await
            && let Ok(saved) = serde_json::from_str::<SavedAccount>(&saved)
            && &saved.directory == directory
        {
            let account = self
                .account_builder()?
                .from_credentials(saved.credentials)
                .await
                .map_err(|e| Failure::acme("could not load the Let's Encrypt account", e))?;
            acme.account = Some(account.clone());
            return Ok(account);
        }

        let (account, credentials) = self
            .account_builder()?
            .create(
                &NewAccount {
                    contact: &[],
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                directory.clone(),
                None,
            )
            .await
            .map_err(|e| Failure::acme("could not create a Let's Encrypt account", e))?;

        let saved = serde_json::to_string(&SavedAccount {
            directory: directory.clone(),
            credentials,
        })
        .map_err(|e| format!("could not serialize the Let's Encrypt account: {e}"))?;
        if let Err(error) = self.inner.store.save_account(&saved).await {
            tracing::warn!(%error, dir = %self.inner.store.dir().display(), "could not save the Let's Encrypt account");
        }

        acme.account = Some(account.clone());
        acme.replace_saved = false;
        Ok(account)
    }

    async fn issue(&self, domain: &str, acme: &mut AcmeAccount) -> Result<LoadedCert, Failure> {
        tracing::info!(%domain, "requesting TLS certificate from Let's Encrypt");
        let mut tokens = ChallengeTokens {
            manager: self,
            tokens: Vec::new(),
        };

        // Checked before ordering, since an order for an unreachable domain leaves a pending
        // authorization behind and counts against Let's Encrypt's limits.
        if self.inner.self_check.enabled {
            self.set_progress(domain, CertStatus::CheckingReachability);
            self.check_reachability(domain, &mut tokens).await?;
        }

        let replaces = self.replaceable_certificate(domain);
        let mut result = self
            .order_with_deadline(domain, acme, replaces.clone(), &mut tokens)
            .await;
        if let Err(failure) = &result
            && acme.note_failure(failure.kind)
        {
            tracing::warn!(%domain, message = %failure.message, "the Let's Encrypt account is no longer valid; registering a new one");
            result = self
                .order_with_deadline(domain, acme, replaces, &mut tokens)
                .await;
            if let Err(failure) = &result {
                acme.note_failure(failure.kind);
            }
        }
        result
    }

    async fn order_with_deadline(
        &self,
        domain: &str,
        acme: &mut AcmeAccount,
        replaces: Option<CertificateIdentifier<'static>>,
        tokens: &mut ChallengeTokens<'_>,
    ) -> Result<LoadedCert, Failure> {
        let timeout = self.inner.issue_timeout;
        tokio::time::timeout(
            timeout,
            self.order_certificate(domain, acme, replaces, tokens),
        )
        .await
        .unwrap_or_else(|_| {
            Err(Failure::from(format!(
                "Let's Encrypt did not respond within {} seconds",
                timeout.as_secs()
            )))
        })
    }

    /// Serves a random token on the tunnel and fetches it over the public internet.
    async fn check_reachability(
        &self,
        domain: &str,
        tokens: &mut ChallengeTokens<'_>,
    ) -> Result<(), Failure> {
        let token = hex::encode(rand::random::<[u8; 24]>());
        let expected = hex::encode(rand::random::<[u8; 24]>());
        tokens.add(token.clone(), expected.clone());

        let config = &self.inner.self_check;
        let host = config.host.as_deref().unwrap_or(domain);
        let result = self_check(host, &token, &expected, config.attempts, config.interval).await;
        tokens.remove(&token);
        result.map_err(|error| {
            Failure::from(format!(
                "http://{domain}{ACME_CHALLENGE_PREFIX} isn't reachable through the tunnel ({error}); \
                 set {SKIP_SELF_CHECK_ENV}=1 if this network can't reach its own public address"
            ))
        })
    }

    fn replaceable_certificate(&self, domain: &str) -> Option<CertificateIdentifier<'static>> {
        if self.inner.ari_unsupported.load(Ordering::Acquire) {
            return None;
        }
        let domains = self.inner.domains.lock().unwrap();
        domains.get(domain).and_then(|state| state.cert_id.clone())
    }

    async fn order_certificate(
        &self,
        domain: &str,
        acme: &mut AcmeAccount,
        replaces: Option<CertificateIdentifier<'static>>,
        tokens: &mut ChallengeTokens<'_>,
    ) -> Result<LoadedCert, Failure> {
        self.set_progress(domain, CertStatus::Requesting);
        let account = self.account(acme).await?;

        let identifiers = [Identifier::Dns(domain.to_string())];
        let refused = |e| Failure::acme("Let's Encrypt refused the certificate order", e);
        let mut order = match replaces {
            Some(replaces) => {
                match account
                    .new_order(&NewOrder::new(&identifiers).replaces(replaces))
                    .await
                {
                    Ok(order) => order,
                    Err(error) => {
                        let failure = refused(error);
                        if failure.kind != FailureKind::Other {
                            return Err(failure);
                        }
                        // The previous certificate may already be replaced, or the server
                        // may not support it; a plain order works either way.
                        tracing::debug!(%domain, message = %failure.message, "could not order a replacement; ordering a new certificate");
                        account
                            .new_order(&NewOrder::new(&identifiers))
                            .await
                            .map_err(refused)?
                    }
                }
            }
            None => account
                .new_order(&NewOrder::new(&identifiers))
                .await
                .map_err(refused)?,
        };

        let mut authorizations = order.authorizations();
        while let Some(authz) = authorizations.next().await {
            let mut authz =
                authz.map_err(|e| Failure::acme("could not load the authorization", e))?;
            match authz.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                status => return Err(format!("the domain authorization is {status:?}").into()),
            }

            let mut challenge = authz
                .challenge(ChallengeType::Http01)
                .ok_or_else(|| "Let's Encrypt offered no HTTP challenge".to_string())?;
            let token = challenge.token.clone();
            let key_auth = challenge.key_authorization().as_str().to_string();
            tokens.add(token, key_auth);

            self.set_progress(domain, CertStatus::Validating);
            challenge
                .set_ready()
                .await
                .map_err(|e| Failure::acme("could not start validation", e))?;
        }

        let status = order
            .poll_ready(&RetryPolicy::new().timeout(Duration::from_secs(120)))
            .await
            .map_err(|e| Failure::acme("validation did not complete", e))?;
        if status != OrderStatus::Ready {
            let mut detail = format!("validation failed (order {status:?})");
            let mut authorizations = order.authorizations();
            while let Some(Ok(authz)) = authorizations.next().await {
                if let Some(problem) = authz
                    .challenges
                    .iter()
                    .find_map(|c| c.error.as_ref())
                    .and_then(|p| p.detail.clone())
                {
                    detail = format!("validation failed: {problem}");
                }
            }
            return Err(detail.into());
        }

        self.set_progress(domain, CertStatus::Issuing);
        let key_pem = order
            .finalize()
            .await
            .map_err(|e| Failure::acme("could not finalize the certificate", e))?;
        let chain_pem = order
            .poll_certificate(&RetryPolicy::new().timeout(Duration::from_secs(60)))
            .await
            .map_err(|e| Failure::acme("could not download the certificate", e))?;

        let stored = StoredCert { chain_pem, key_pem };
        let cert = load_certificate(&stored)?;
        if let Err(error) = self.inner.store.save_cert(domain, &stored).await {
            // The certificate still works for this run; it is requested again after a restart.
            tracing::error!(%domain, %error, dir = %self.inner.store.dir().display(), "could not save the TLS certificate");
        }
        Ok(cert)
    }

    async fn fetch_renewal_info(
        &self,
        acme: &mut AcmeAccount,
        cert_id: &CertificateIdentifier<'_>,
    ) -> RenewalInfo {
        // Renewal information isn't worth registering an account for; without a usable one
        // the 30-day fallback applies until the next certificate request creates it.
        if acme.account.is_none() && (acme.replace_saved || !self.has_saved_account().await) {
            return RenewalInfo::Failed("no Let's Encrypt account is saved yet".to_string());
        }
        let account = match tokio::time::timeout(ARI_TIMEOUT, self.account(acme)).await {
            Ok(Ok(account)) => account,
            Ok(Err(failure)) => {
                acme.note_failure(failure.kind);
                return RenewalInfo::Failed(failure.message);
            }
            Err(_) => return RenewalInfo::Failed("loading the account timed out".to_string()),
        };
        match tokio::time::timeout(ARI_TIMEOUT, account.renewal_info(cert_id)).await {
            Ok(Ok((info, recheck))) => RenewalInfo::Window {
                start: nanos_to_millis(info.suggested_window.start.unix_timestamp_nanos()),
                end: nanos_to_millis(info.suggested_window.end.unix_timestamp_nanos()),
                recheck_ms: u64::try_from(recheck.as_millis()).unwrap_or(u64::MAX),
            },
            Ok(Err(instant_acme::Error::Unsupported(_))) => RenewalInfo::Unsupported,
            Ok(Err(error)) => RenewalInfo::Failed(error.to_string()),
            Err(_) => RenewalInfo::Failed("the request timed out".to_string()),
        }
    }

    fn apply_renewal_info(
        &self,
        domain: &str,
        cert_id: &CertificateIdentifier<'_>,
        info: RenewalInfo,
        now: u64,
    ) {
        if info == RenewalInfo::Unsupported {
            tracing::debug!(
                "the ACME server has no renewal information; renewing 30 days before expiry"
            );
            self.inner.ari_unsupported.store(true, Ordering::Release);
            return;
        }

        let mut domains = self.inner.domains.lock().unwrap();
        let Some(state) = domains.get_mut(domain) else {
            return;
        };
        // The certificate may have been replaced while the request was running.
        if state.cert_id.as_ref() != Some(cert_id) {
            return;
        }

        match info {
            RenewalInfo::Window {
                start,
                end,
                recheck_ms,
            } => {
                // The time is picked once per window so rechecks don't pull renewal earlier.
                if state.ari_window != Some((start, end)) || state.ari_renew_at.is_none() {
                    let renew_at = if end > start {
                        rand::random_range(start..=end)
                    } else {
                        start
                    };
                    tracing::debug!(%domain, renew_at, "scheduled renewal from the ACME server's renewal information");
                    state.ari_window = Some((start, end));
                    state.ari_renew_at = Some(renew_at);
                }
                state.ari_check_at = now + recheck_ms.clamp(ARI_MIN_RECHECK_MS, ARI_MAX_RECHECK_MS);
            }
            RenewalInfo::Failed(error) => {
                tracing::debug!(%domain, %error, "could not fetch renewal information");
                state.ari_check_at = now + ARI_ERROR_RECHECK_MS;
            }
            RenewalInfo::Unsupported => {}
        }
    }
}

fn nanos_to_millis(nanos: i128) -> u64 {
    u64::try_from(nanos / 1_000_000).unwrap_or(0)
}

fn env_flag(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.trim();
        value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
    })
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedAccount {
    directory: String,
    credentials: AccountCredentials,
}

/// Removes challenge responses once an attempt ends, however it ends.
struct ChallengeTokens<'a> {
    manager: &'a CertManager,
    tokens: Vec<String>,
}

impl ChallengeTokens<'_> {
    fn add(&mut self, token: String, key_auth: String) {
        self.manager
            .inner
            .challenges
            .lock()
            .unwrap()
            .insert(token.clone(), key_auth);
        self.tokens.push(token);
    }

    fn remove(&mut self, token: &str) {
        self.manager.inner.challenges.lock().unwrap().remove(token);
        self.tokens.retain(|t| t != token);
    }
}

impl Drop for ChallengeTokens<'_> {
    fn drop(&mut self) {
        let mut challenges = self.manager.inner.challenges.lock().unwrap();
        for token in &self.tokens {
            challenges.remove(token);
        }
    }
}

/// Fetches a challenge token over the public internet, the way Let's Encrypt will.
async fn self_check(
    host: &str,
    token: &str,
    expected: &str,
    attempts: u32,
    interval: Duration,
) -> Result<(), String> {
    let client = Client::builder(TokioExecutor::new()).build_http::<Empty<bytes::Bytes>>();
    let uri: hyper::Uri = format!("http://{host}{ACME_CHALLENGE_PREFIX}{token}")
        .parse()
        .map_err(|e| format!("invalid challenge URL: {e}"))?;

    let mut last_error = String::new();
    for attempt in 1..=attempts {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let response = client
                .get(uri.clone())
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("got HTTP {status}"));
            }
            // The answer is a short token. Anything longer is wrong, and is rejected
            // without being read so a hostile responder can't make the agent buffer it.
            let declared_length = response
                .headers()
                .get(hyper::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            if declared_length.is_some_and(|length| length > expected.len() as u64) {
                return Err(format!("got HTTP {status} with an unexpected body"));
            }
            let body = Limited::new(response.into_body(), expected.len())
                .collect()
                .await
                .map_err(|e| format!("reading the response failed: {e}"))?
                .to_bytes();
            if body.as_ref() == expected.as_bytes() {
                Ok(())
            } else {
                Err(format!("got HTTP {status} with an unexpected body"))
            }
        })
        .await
        .unwrap_or_else(|_| Err("request timed out".to_string()));

        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                tracing::debug!(%host, attempt, %error, "reachability self-check failed");
                last_error = error;
            }
        }
        if attempt < attempts {
            tokio::time::sleep(interval).await;
        }
    }
    Err(last_error)
}

/// Builds a TLS acceptor from a stored certificate and returns it with the expiry time
/// (milliseconds since the Unix epoch) of the leaf certificate.
pub fn build_acceptor(stored: &StoredCert) -> Result<(TlsAcceptor, u64), String> {
    load_certificate(stored).map(|cert| (cert.acceptor, cert.expires_at))
}

fn load_certificate(stored: &StoredCert) -> Result<LoadedCert, String> {
    let chain = CertificateDer::pem_slice_iter(stored.chain_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid certificate PEM: {e}"))?;
    let leaf = chain
        .first()
        .ok_or_else(|| "the certificate file is empty".to_string())?;
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|e| format!("invalid certificate: {e}"))?;
    let expires_at = (parsed.validity().not_after.timestamp().max(0) as u64) * 1000;
    let cert_id = certificate_identifier(&parsed);

    let key = PrivateKeyDer::from_pem_slice(stored.key_pem.as_bytes())
        .map_err(|e| format!("invalid private key PEM: {e}"))?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS setup failed: {e}"))?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|e| format!("the certificate and key don't match: {e}"))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Ok(LoadedCert {
        acceptor: TlsAcceptor::from(Arc::new(config)),
        expires_at,
        cert_id,
    })
}

/// The ARI identifier of a certificate: its issuer's key identifier and its serial number.
/// Certificates without an authority key identifier can't be looked up.
fn certificate_identifier(
    cert: &x509_parser::certificate::X509Certificate<'_>,
) -> Option<CertificateIdentifier<'static>> {
    let key_id = cert
        .iter_extensions()
        .find_map(|ext| match ext.parsed_extension() {
            ParsedExtension::AuthorityKeyIdentifier(aki) => {
                aki.key_identifier.as_ref().map(|id| id.0)
            }
            _ => None,
        })?;
    Some(
        CertificateIdentifier::new(
            Der::from_slice(key_id),
            Der::from_slice(cert.tbs_certificate.raw_serial()),
        )
        .into_owned(),
    )
}

/// Accepts only names Let's Encrypt can issue for, as a lowercase domain.
pub fn certificate_domain(display_address: &str) -> Option<String> {
    let host = display_address
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = host.split('/').next().unwrap_or(host);
    let host = match host.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => host,
        _ => host,
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    is_valid_domain(&host).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn self_signed(domain: &str) -> StoredCert {
        let cert = rcgen::generate_simple_self_signed(vec![domain.to_string()]).unwrap();
        StoredCert {
            chain_pem: cert.cert.pem(),
            key_pem: cert.signing_key.serialize_pem(),
        }
    }

    /// A certificate with an authority key identifier, so it has an ARI identifier.
    fn with_key_identifier(domain: &str) -> StoredCert {
        let mut params = rcgen::CertificateParams::new(vec![domain.to_string()]).unwrap();
        params.use_authority_key_identifier_extension = true;
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        StoredCert {
            chain_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "playit-cert-manager-{name}-{}-{}",
            std::process::id(),
            now_milli()
        ))
    }

    fn unused_manager() -> CertManager {
        CertManager::with_acme_server(
            CertStore::new(std::env::temp_dir().join("unused")),
            "https://127.0.0.1:1/dir".to_string(),
            None,
            SelfCheck::new(false),
        )
    }

    impl CertManager {
        fn with_issue_timeout(mut self, timeout: Duration) -> Self {
            Arc::get_mut(&mut self.inner).unwrap().issue_timeout = timeout;
            self
        }

        fn with_state<R>(&self, domain: &str, f: impl FnOnce(&mut DomainState) -> R) -> R {
            f(self.inner.domains.lock().unwrap().get_mut(domain).unwrap())
        }
    }

    fn problem(problem_type: &str, detail: &str) -> Problem {
        serde_json::from_value(serde_json::json!({
            "type": problem_type,
            "detail": detail,
            "status": 400,
        }))
        .unwrap()
    }

    async fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Accepts connections and never answers, like a stalled ACME server.
    async fn silent_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        port
    }

    /// Closes every connection right away and counts them.
    async fn refusing_server() -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = count.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                drop(socket);
            }
        });
        (port, count)
    }

    /// Answers challenge requests from the manager's challenge map, as the tunnel would, or
    /// with 404 when `reachable` is false.
    fn serve_challenges(listener: TcpListener, manager: CertManager, reachable: bool) {
        serve_challenges_when(
            listener,
            manager,
            Arc::new(std::sync::atomic::AtomicBool::new(reachable)),
        );
    }

    fn serve_challenges_when(
        listener: TcpListener,
        manager: CertManager,
        reachable: Arc<std::sync::atomic::AtomicBool>,
    ) {
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let manager = manager.clone();
                let reachable = reachable.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let request = String::from_utf8_lossy(&request);
                    let path = request.split(' ').nth(1).unwrap_or("");
                    let body = path
                        .strip_prefix(ACME_CHALLENGE_PREFIX)
                        .and_then(|token| manager.challenge_response(token))
                        .filter(|_| reachable);
                    let response = match body {
                        Some(body) => format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        ),
                        None => "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_string(),
                    };
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
    }

    /// Answers every request with `head`, then streams `chunk` until the client hangs up.
    async fn flooding_server(head: &'static str, chunk: &'static [u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    while socket.write_all(chunk).await.is_ok() {}
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn self_check_stops_reading_an_oversized_body() {
        let chunk: &'static [u8] = Box::leak(
            format!("4000\r\n{}\r\n", "x".repeat(0x4000))
                .into_bytes()
                .into_boxed_slice(),
        );
        let port = flooding_server(
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n",
            chunk,
        )
        .await;
        let started = std::time::Instant::now();
        let error = self_check(
            &format!("127.0.0.1:{port}"),
            "token",
            "token.thumbprint",
            1,
            Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert!(error.contains("reading the response failed"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn self_check_rejects_a_long_declared_length_without_reading_it() {
        let port = flooding_server(
            "HTTP/1.1 200 OK\r\ncontent-length: 1073741824\r\n\r\n",
            &[b'x'; 0x4000],
        )
        .await;
        let error = self_check(
            &format!("127.0.0.1:{port}"),
            "token",
            "token.thumbprint",
            1,
            Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert!(error.contains("unexpected body"), "{error}");
    }

    #[test]
    fn certificate_domain_strips_ports_and_schemes() {
        assert_eq!(
            certificate_domain("abc.playit.plus").as_deref(),
            Some("abc.playit.plus")
        );
        assert_eq!(
            certificate_domain("https://Site.Example.com:443/").as_deref(),
            Some("site.example.com")
        );
        assert_eq!(certificate_domain("147.185.221.1:443"), None);
        assert_eq!(certificate_domain("localhost"), None);
    }

    #[test]
    fn build_acceptor_reads_expiry() {
        let (_, expires_at) = build_acceptor(&self_signed("site.example.com")).unwrap();
        // rcgen's default validity runs to 4096.
        assert!(expires_at > now_milli() + RENEW_BEFORE_MS);
    }

    #[test]
    fn build_acceptor_rejects_mismatched_key() {
        let a = self_signed("a.example.com");
        let b = self_signed("b.example.com");
        let mixed = StoredCert {
            chain_pem: a.chain_pem,
            key_pem: b.key_pem,
        };
        assert!(build_acceptor(&mixed).is_err());
    }

    #[test]
    fn certificate_identifier_needs_an_authority_key_identifier() {
        let cert = load_certificate(&with_key_identifier("site.example.com")).unwrap();
        let id = cert.cert_id.expect("identifier");
        assert!(!id.authority_key_identifier.is_empty());
        assert!(!id.serial.is_empty());
    }

    #[tokio::test]
    async fn stored_certificate_is_loaded_without_requesting() {
        let dir = temp_dir("stored");
        let store = CertStore::new(dir.clone());
        store
            .save_cert("site.example.com", &self_signed("site.example.com"))
            .await
            .unwrap();

        let manager = CertManager::new(store);
        manager.set_domains(["site.example.com".to_string()]);
        assert_eq!(
            manager.status("site.example.com"),
            Some(CertStatus::Preparing)
        );

        manager.load_stored().await;
        assert!(manager.status("site.example.com").unwrap().is_ready());
        assert!(manager.acceptor("site.example.com").is_some());
        assert_eq!(manager.next_domain_to_issue(now_milli()), None);

        manager.set_domains(Vec::new());
        assert_eq!(manager.status("site.example.com"), None);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_certificate_waits_for_the_agent_to_connect() {
        let manager = unused_manager();
        manager.set_domains(["site.example.com".to_string()]);
        manager.with_state("site.example.com", |state| state.loaded_from_disk = true);

        assert_eq!(manager.next_domain_to_issue(now_milli()), None);
        manager.set_active(true);
        assert_eq!(
            manager.next_domain_to_issue(now_milli()).as_deref(),
            Some("site.example.com")
        );
    }

    #[test]
    fn inactive_worker_sleeps_until_woken() {
        let manager = unused_manager();
        manager.set_domains(["site.example.com".to_string()]);
        manager.with_state("site.example.com", |state| state.loaded_from_disk = true);

        // The domain needs a certificate now, but nothing can happen until the agent connects.
        assert_eq!(manager.next_wake_in(now_milli()), IDLE_CHECK_MS);

        manager.set_active(true);
        let now = now_milli();
        manager.with_state("site.example.com", |state| state.next_attempt = now + 5_000);
        assert_eq!(manager.next_wake_in(now), 5_000);
    }

    #[test]
    fn failures_back_off_and_keep_a_valid_certificate() {
        let manager = unused_manager();
        manager.set_domains(["site.example.com".to_string()]);

        manager.finish_attempt("site.example.com", Err("boom".to_string().into()));
        match manager.status("site.example.com").unwrap() {
            CertStatus::Failed { message, retry_at } => {
                assert_eq!(message, "boom");
                assert!(retry_at >= now_milli() + 59_000);
            }
            other => panic!("unexpected status {other:?}"),
        }

        let (acceptor, expires_at) = build_acceptor(&self_signed("site.example.com")).unwrap();
        manager.with_state("site.example.com", |state| {
            state.acceptor = Some(acceptor);
            state.expires_at = Some(expires_at);
        });
        manager.finish_attempt("site.example.com", Err("renewal failed".to_string().into()));
        assert!(manager.status("site.example.com").unwrap().is_ready());
        assert!(manager.acceptor("site.example.com").is_some());
    }

    #[test]
    fn expired_certificate_is_not_served() {
        let manager = unused_manager();
        manager.set_domains(["site.example.com".to_string()]);
        let (acceptor, _) = build_acceptor(&self_signed("site.example.com")).unwrap();
        let expired = now_milli() - 1_000;
        manager.with_state("site.example.com", |state| {
            state.acceptor = Some(acceptor);
            state.expires_at = Some(expired);
            state.status = CertStatus::Ready {
                expires_at: expired,
                renewing: false,
            };
        });

        assert!(manager.acceptor("site.example.com").is_none());
        match manager.status("site.example.com").unwrap() {
            CertStatus::Failed { message, .. } => assert!(message.contains("expired")),
            other => panic!("unexpected status {other:?}"),
        }

        // A renewal attempt reports its own progress rather than the stale certificate.
        manager.set_progress("site.example.com", CertStatus::Requesting);
        assert_eq!(
            manager.status("site.example.com"),
            Some(CertStatus::Requesting)
        );
        manager.finish_attempt("site.example.com", Err("still failing".to_string().into()));
        assert!(matches!(
            manager.status("site.example.com"),
            Some(CertStatus::Failed { .. })
        ));
        assert!(manager.acceptor("site.example.com").is_none());
    }

    #[test]
    fn removed_domain_keeps_its_backoff() {
        let manager = unused_manager();
        let domain = "site.example.com";
        manager.set_domains([domain.to_string()]);
        manager.finish_attempt(domain, Err("one".to_string().into()));
        manager.finish_attempt(domain, Err("two".to_string().into()));
        let next_attempt = manager.with_state(domain, |state| state.next_attempt);

        // Toggling the tunnel off and on must not allow an immediate retry.
        manager.set_domains(Vec::new());
        manager.set_domains([domain.to_string()]);
        manager.set_active(true);
        manager.with_state(domain, |state| {
            assert_eq!(state.failures, 2);
            assert_eq!(state.next_attempt, next_attempt);
            state.loaded_from_disk = true;
        });
        assert!(matches!(
            manager.status(domain),
            Some(CertStatus::Failed { message, .. }) if message == "two"
        ));
        assert_eq!(manager.next_domain_to_issue(now_milli()), None);

        // The next failure continues the escalation.
        manager.finish_attempt(domain, Err("three".to_string().into()));
        let retry_in = manager.with_state(domain, |state| state.next_attempt) - now_milli();
        assert!(retry_in > RETRY_DELAYS_SECS[1] * SECOND_MS);

        // Old entries are forgotten.
        manager.set_domains(Vec::new());
        manager
            .inner
            .removed
            .lock()
            .unwrap()
            .get_mut(domain)
            .unwrap()
            .removed_at = now_milli() - FORGET_REMOVED_MS - 1;
        manager.set_domains([domain.to_string()]);
        manager.with_state(domain, |state| {
            assert_eq!(state.failures, 0);
            assert_eq!(state.next_attempt, 0);
        });

        // A success clears the backoff, so nothing is remembered on removal.
        let cert = load_certificate(&self_signed(domain)).unwrap();
        manager.finish_attempt(domain, Ok(cert));
        manager.set_domains(Vec::new());
        assert!(manager.inner.removed.lock().unwrap().is_empty());
    }

    #[test]
    fn rate_limits_use_retry_after_within_bounds() {
        let now = 1_700_000_000_000;
        let regular = |failures| retry_delay_ms(failures, FailureKind::Other, now);
        assert_eq!(regular(1), 60_000);
        assert_eq!(regular(2), 5 * 60_000);
        assert_eq!(regular(99), 3 * HOUR_MS);
        assert_eq!(retry_delay_ms(1, FailureKind::AccountInvalid, now), 60_000);

        let limited = |retry_at| retry_delay_ms(1, FailureKind::RateLimited { retry_at }, now);
        assert_eq!(limited(Some(now + 2 * DAY_MS)), 2 * DAY_MS);
        assert_eq!(limited(Some(now + 1_000)), RATE_LIMIT_MIN_WAIT_MS);
        assert_eq!(limited(Some(now - 1_000)), RATE_LIMIT_MIN_WAIT_MS);
        assert_eq!(limited(Some(now + 30 * DAY_MS)), RATE_LIMIT_MAX_WAIT_MS);
        assert_eq!(limited(None), RATE_LIMIT_DEFAULT_WAIT_MS);
        assert_eq!(
            retry_delay_ms(5, FailureKind::RateLimited { retry_at: None }, now),
            3 * HOUR_MS
        );
    }

    #[test]
    fn rate_limit_problems_are_recognized() {
        let detail = "too many certificates (5) already issued for this exact set of identifiers \
                      in the last 168h0m0s, retry after 2025-01-25 21:57:49 UTC: see \
                      https://letsencrypt.org/docs/rate-limits/";
        let expected = chrono::DateTime::parse_from_rfc3339("2025-01-25T21:57:49Z")
            .unwrap()
            .timestamp_millis() as u64;
        assert_eq!(retry_after_from_detail(detail), Some(expected));
        assert_eq!(retry_after_from_detail("Retry After soon"), None);
        assert_eq!(retry_after_from_detail("no hint"), None);

        let failure = Failure::acme(
            "Let's Encrypt refused the certificate order",
            instant_acme::Error::Api(problem("urn:ietf:params:acme:error:rateLimited", detail)),
        );
        assert_eq!(
            failure.kind,
            FailureKind::RateLimited {
                retry_at: Some(expected)
            }
        );
        assert!(failure.message.starts_with("rate limited by Let's Encrypt"));

        // The status shows the rate limit and the wait it implies.
        let manager = unused_manager();
        manager.set_domains(["site.example.com".to_string()]);
        manager.finish_attempt("site.example.com", Err(failure));
        match manager.status("site.example.com").unwrap() {
            CertStatus::Failed { message, retry_at } => {
                assert!(message.contains("rate limited"));
                assert!(retry_at >= now_milli() + RATE_LIMIT_MIN_WAIT_MS - 1_000);
            }
            other => panic!("unexpected status {other:?}"),
        }
    }

    #[test]
    fn account_problems_are_recognized() {
        let kind = |problem_type, detail| classify_problem(&problem(problem_type, detail));
        assert_eq!(
            kind(
                "urn:ietf:params:acme:error:accountDoesNotExist",
                "Account \"https://acme/acct/1\" not found"
            ),
            FailureKind::AccountInvalid
        );
        assert_eq!(
            kind(
                "urn:ietf:params:acme:error:unauthorized",
                "Account is not valid, has status \"deactivated\""
            ),
            FailureKind::AccountInvalid
        );
        // Other refusals keep the account.
        assert_eq!(
            kind(
                "urn:ietf:params:acme:error:unauthorized",
                "Error finalizing order :: identifier is not allowed"
            ),
            FailureKind::Other
        );
        assert_eq!(
            kind(
                "urn:ietf:params:acme:error:serverInternal",
                "account lookup failed"
            ),
            FailureKind::Other
        );
        assert_eq!(
            kind(
                "urn:ietf:params:acme:error:badNonce",
                "JWS has an invalid anti-replay nonce"
            ),
            FailureKind::Other
        );
    }

    #[test]
    fn only_account_failures_replace_the_saved_account() {
        let mut acme = AcmeAccount::default();
        assert!(!acme.note_failure(FailureKind::Other));
        assert!(!acme.note_failure(FailureKind::RateLimited { retry_at: None }));
        assert!(!acme.replace_saved);

        assert!(acme.note_failure(FailureKind::AccountInvalid));
        assert!(acme.replace_saved);
        assert!(acme.account.is_none());
    }

    #[test]
    fn skip_self_check_flag() {
        assert!(env_flag(Some("1")));
        assert!(env_flag(Some(" true ")));
        assert!(env_flag(Some("YES")));
        assert!(!env_flag(Some("0")));
        assert!(!env_flag(Some("")));
        assert!(!env_flag(None));
    }

    #[test]
    fn renewal_falls_back_to_30_days_without_renewal_info() {
        let manager = unused_manager();
        let domain = "site.example.com";
        manager.set_domains([domain.to_string()]);
        manager.set_active(true);
        let cert = load_certificate(&with_key_identifier(domain)).unwrap();
        let expires_at = cert.expires_at;
        let cert_id = cert.cert_id.clone().unwrap();
        manager.finish_attempt(domain, Ok(cert));

        let renew_at = || manager.with_state(domain, |state| state.renew_at());
        assert_eq!(renew_at(), Some(expires_at - RENEW_BEFORE_MS));

        let now = now_milli();
        assert_eq!(
            manager.next_renewal_info_check(now),
            Some((domain.to_string(), cert_id.clone()))
        );

        // A failed lookup keeps the fallback and tries again later.
        manager.apply_renewal_info(domain, &cert_id, RenewalInfo::Failed("boom".into()), now);
        assert_eq!(renew_at(), Some(expires_at - RENEW_BEFORE_MS));
        assert_eq!(manager.next_renewal_info_check(now), None);
        assert_eq!(
            manager.with_state(domain, |state| state.ari_check_at),
            now + ARI_ERROR_RECHECK_MS
        );

        // A server without renewal information is not asked again and gets no "replaces".
        manager.apply_renewal_info(domain, &cert_id, RenewalInfo::Unsupported, now);
        assert_eq!(renew_at(), Some(expires_at - RENEW_BEFORE_MS));
        assert_eq!(manager.next_renewal_info_check(u64::MAX - 1), None);
        assert_eq!(manager.replaceable_certificate(domain), None);
        assert!(manager.next_wake_in(now) <= IDLE_CHECK_MS);
    }

    #[test]
    fn renewal_info_window_sets_the_renewal_time() {
        let manager = unused_manager();
        let domain = "site.example.com";
        manager.set_domains([domain.to_string()]);
        manager.set_active(true);
        let cert = load_certificate(&with_key_identifier(domain)).unwrap();
        let expires_at = cert.expires_at;
        let cert_id = cert.cert_id.clone().unwrap();
        manager.finish_attempt(domain, Ok(cert));
        assert_eq!(
            manager.replaceable_certificate(domain),
            Some(cert_id.clone())
        );

        let now = now_milli();
        let (start, end) = (now + DAY_MS, now + 2 * DAY_MS);
        let window = RenewalInfo::Window {
            start,
            end,
            recheck_ms: 0,
        };
        manager.apply_renewal_info(domain, &cert_id, window.clone(), now);
        let renew_at = manager
            .with_state(domain, |state| state.renew_at())
            .unwrap();
        assert!((start..=end).contains(&renew_at));
        assert_eq!(
            manager.with_state(domain, |state| state.ari_check_at),
            now + ARI_MIN_RECHECK_MS
        );
        // The same window keeps the time already picked.
        manager.apply_renewal_info(domain, &cert_id, window, now);
        assert_eq!(
            manager.with_state(domain, |state| state.renew_at()),
            Some(renew_at)
        );
        assert!(!manager.with_state(domain, |state| state.needs_certificate(now)));

        // A window that has started (e.g. the certificate is being revoked) renews now.
        let started = RenewalInfo::Window {
            start: now - 1_000,
            end: now - 1_000,
            recheck_ms: 0,
        };
        manager.apply_renewal_info(domain, &cert_id, started, now);
        assert!(manager.with_state(domain, |state| state.needs_certificate(now)));

        // A window past the expiry is pulled in so there is time to retry.
        let late = RenewalInfo::Window {
            start: expires_at + DAY_MS,
            end: expires_at + DAY_MS,
            recheck_ms: 0,
        };
        manager.apply_renewal_info(domain, &cert_id, late, now);
        assert_eq!(
            manager.with_state(domain, |state| state.renew_at()),
            Some(expires_at - DAY_MS)
        );

        // Information about a certificate that was since replaced is ignored.
        let replacement = load_certificate(&with_key_identifier(domain)).unwrap();
        manager.finish_attempt(domain, Ok(replacement));
        manager.apply_renewal_info(
            domain,
            &cert_id,
            RenewalInfo::Window {
                start: now,
                end: now,
                recheck_ms: 0,
            },
            now,
        );
        assert_eq!(manager.with_state(domain, |state| state.ari_renew_at), None);
    }

    #[tokio::test]
    async fn stalled_request_times_out_and_stored_certificates_still_load() {
        let port = silent_server().await;
        let dir = temp_dir("stalled");
        let store = CertStore::new(dir.clone());
        store
            .save_cert("stored.example.com", &self_signed("stored.example.com"))
            .await
            .unwrap();

        let timeout = Duration::from_secs(3);
        let manager = CertManager::with_acme_server(
            store,
            format!("https://127.0.0.1:{port}/dir"),
            None,
            SelfCheck::new(false),
        )
        .with_issue_timeout(timeout);
        manager.set_domains(["site.example.com".to_string()]);
        manager.set_active(true);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(manager.clone().run(cancel.clone()));

        wait_for("the request to start", || {
            manager.status("site.example.com") == Some(CertStatus::Requesting)
        })
        .await;
        let started = std::time::Instant::now();

        manager.set_domains([
            "site.example.com".to_string(),
            "stored.example.com".to_string(),
        ]);
        wait_for("the stored certificate", || {
            manager.acceptor("stored.example.com").is_some()
        })
        .await;
        assert!(started.elapsed() < timeout);
        assert_eq!(
            manager.status("site.example.com"),
            Some(CertStatus::Requesting)
        );

        wait_for("the attempt to time out", || {
            matches!(
                manager.status("site.example.com"),
                Some(CertStatus::Failed { message, .. }) if message.contains("did not respond")
            )
        })
        .await;

        cancel.cancel();
        worker.await.unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn reachability_is_checked_before_ordering() {
        let (acme_port, acme_connections) = refusing_server().await;
        let directory = format!("https://127.0.0.1:{acme_port}/dir");
        let dir = temp_dir("self-check");

        for reachable in [false, true] {
            acme_connections.store(0, Ordering::SeqCst);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let manager = CertManager::with_acme_server(
                CertStore::new(dir.clone()),
                directory.clone(),
                None,
                SelfCheck {
                    enabled: true,
                    host: Some(format!("127.0.0.1:{port}")),
                    attempts: 2,
                    interval: Duration::from_millis(10),
                },
            );
            serve_challenges(listener, manager.clone(), reachable);
            manager.set_domains(["site.example.com".to_string()]);

            let mut acme = AcmeAccount::default();
            let failure = match manager.issue("site.example.com", &mut acme).await {
                Ok(_) => panic!("issued without an ACME server"),
                Err(failure) => failure,
            };
            assert!(manager.inner.challenges.lock().unwrap().is_empty());
            if reachable {
                assert!(
                    failure
                        .message
                        .contains("could not create a Let's Encrypt account"),
                    "{failure:?}"
                );
                assert!(acme_connections.load(Ordering::SeqCst) > 0);
            } else {
                assert!(failure.message.contains("isn't reachable"), "{failure:?}");
                assert!(failure.message.contains(SKIP_SELF_CHECK_ENV));
                assert_eq!(acme_connections.load(Ordering::SeqCst), 0);
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn reachability_check_does_not_count_against_the_acme_deadline() {
        let acme_port = silent_server().await;
        let dir = temp_dir("deadline-scope");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let deadline = Duration::from_millis(400);
        let manager = CertManager::with_acme_server(
            CertStore::new(dir.clone()),
            format!("https://127.0.0.1:{acme_port}/dir"),
            None,
            SelfCheck {
                enabled: true,
                host: Some(format!("127.0.0.1:{port}")),
                attempts: 20,
                interval: Duration::from_millis(100),
            },
        )
        .with_issue_timeout(deadline);
        let reachable = Arc::new(std::sync::atomic::AtomicBool::new(false));
        serve_challenges_when(listener, manager.clone(), reachable.clone());
        manager.set_domains(["site.example.com".to_string()]);

        // The tunnel becomes reachable only after longer than the ACME deadline.
        let slow_check = Duration::from_millis(800);
        tokio::spawn(async move {
            tokio::time::sleep(slow_check).await;
            reachable.store(true, Ordering::SeqCst);
        });

        let started = std::time::Instant::now();
        let mut acme = AcmeAccount::default();
        let failure = match manager.issue("site.example.com", &mut acme).await {
            Ok(_) => panic!("issued from a silent ACME server"),
            Err(failure) => failure,
        };
        assert!(failure.message.contains("did not respond"), "{failure:?}");
        assert!(started.elapsed() >= slow_check + deadline);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failure_during_removal_keeps_the_backoff() {
        let manager = CertManager::new(CertStore::new(temp_dir("removed-in-flight")));
        manager.set_domains(["site.example.com".to_string()]);
        manager.set_domains(Vec::new());
        manager.finish_attempt("site.example.com", Err(Failure::from("boom".to_string())));

        manager.set_domains(["site.example.com".to_string()]);
        let next_attempt = manager.inner.domains.lock().unwrap()["site.example.com"].next_attempt;
        assert!(next_attempt >= now_milli() + 59_000);
        assert_eq!(manager.next_domain_to_issue(now_milli()), None);
    }

    #[tokio::test]
    async fn renewal_info_does_not_register_an_account() {
        let (acme_port, acme_connections) = refusing_server().await;
        let manager = CertManager::with_acme_server(
            CertStore::new(temp_dir("ari-no-account")),
            format!("https://127.0.0.1:{acme_port}/dir"),
            None,
            SelfCheck::new(false),
        );
        let cert_id = CertificateIdentifier {
            authority_key_identifier: "aki".into(),
            serial: "serial".into(),
        };
        let mut acme = AcmeAccount::default();
        let info = manager.fetch_renewal_info(&mut acme, &cert_id).await;
        assert!(matches!(info, RenewalInfo::Failed(_)), "{info:?}");
        assert_eq!(acme_connections.load(Ordering::SeqCst), 0);
    }

    /// Runs the full ACME exchange against Pebble, Let's Encrypt's test server, which is
    /// told to accept every challenge. Run with `cargo test -- --ignored pebble`.
    #[tokio::test]
    #[ignore = "needs docker to run the Pebble ACME test server"]
    async fn issues_and_reloads_a_certificate_from_pebble() {
        use std::process::Command;

        let docker = |args: &[&str]| {
            let output = Command::new("docker").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "docker {args:?} failed: {output:?}"
            );
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        let container = docker(&[
            "run",
            "-d",
            "--rm",
            "-p",
            "127.0.0.1::14000",
            "-e",
            "PEBBLE_VA_ALWAYS_VALID=1",
            "-e",
            "PEBBLE_WFE_NONCEREJECT=0",
            "ghcr.io/letsencrypt/pebble:latest",
            "-config",
            "test/config/pebble-config.json",
        ]);
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", &self.0])
                    .output();
            }
        }
        let _cleanup = Cleanup(container.clone());

        let dir = temp_dir("pebble");
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.join("pebble.minica.pem");
        docker(&[
            "cp",
            &format!("{container}:/test/certs/pebble.minica.pem"),
            root.to_str().unwrap(),
        ]);
        let port = docker(&["port", &container, "14000"])
            .rsplit(':')
            .next()
            .unwrap()
            .to_string();
        let directory = format!("https://localhost:{port}/dir");

        let new_manager = || {
            CertManager::with_acme_server(
                CertStore::new(dir.join("certs")),
                directory.clone(),
                Some(root.clone()),
                SelfCheck::new(false),
            )
        };
        let domain = "site.example.com";
        let manager = new_manager();
        manager.set_domains([domain.to_string()]);
        manager.set_active(true);
        manager.load_stored().await;

        let mut account = AcmeAccount::default();
        let mut result = Err(Failure::from(String::new()));
        // Pebble needs a moment to start listening.
        for _ in 0..20 {
            result = manager.issue(domain, &mut account).await;
            if result.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let cert = result.expect("pebble issued a certificate");
        assert!(cert.expires_at > now_milli());
        assert!(manager.inner.challenges.lock().unwrap().is_empty());
        manager.finish_attempt(domain, Ok(cert));

        // Pebble offers renewal information, and the renewal names the certificate it replaces.
        let (_, cert_id) = manager
            .next_renewal_info_check(now_milli())
            .expect("the certificate has an ARI identifier");
        let info = manager.fetch_renewal_info(&mut account, &cert_id).await;
        assert!(matches!(info, RenewalInfo::Window { .. }), "{info:?}");
        manager.apply_renewal_info(domain, &cert_id, info, now_milli());
        assert!(manager.with_state(domain, |state| state.ari_renew_at.is_some()));
        let renewed = manager
            .issue(domain, &mut account)
            .await
            .expect("pebble renewed the certificate");
        assert_ne!(renewed.cert_id, Some(cert_id));
        manager.finish_attempt(domain, Ok(renewed));

        // A new manager (as after a restart) loads the stored certificate and account.
        let restarted = new_manager();
        restarted.set_domains([domain.to_string()]);
        restarted.load_stored().await;
        assert!(restarted.acceptor(domain).is_some());
        let mut account = AcmeAccount::default();
        let saved = restarted
            .account(&mut account)
            .await
            .expect("stored account loads");

        // A deactivated account is replaced with a new one instead of failing forever.
        saved.deactivate().await.expect("deactivate the account");
        let saved_credentials = restarted.store().load_account().await.unwrap();
        let recovered = new_manager();
        recovered.set_domains([domain.to_string()]);
        recovered.load_stored().await;
        let mut account = AcmeAccount::default();
        recovered
            .issue(domain, &mut account)
            .await
            .expect("a new account was registered");
        assert!(!account.replace_saved);
        assert_ne!(
            recovered.store().load_account().await.unwrap(),
            saved_credentials
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
