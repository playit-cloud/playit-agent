use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::{BodyExt, Empty};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt,
    NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use super::store::{CertStore, StoredCert, is_valid_domain};
use crate::utils::now_milli;

/// Certificates are renewed once they have fewer than this many days left. Let's Encrypt
/// issues 90 day certificates and recommends renewing at two thirds of their lifetime.
const RENEW_BEFORE_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// How often the worker re-checks expiry when nothing else wakes it.
const IDLE_CHECK_MS: u64 = 6 * 60 * 60 * 1000;
const RETRY_DELAYS_SECS: [u64; 5] = [60, 5 * 60, 15 * 60, 60 * 60, 3 * 60 * 60];
/// Asking Let's Encrypt to validate a domain it can't reach counts against its failed
/// validation limit, so the agent first fetches the challenge through the tunnel itself.
const SELF_CHECK_ATTEMPTS: u32 = 12;
const SELF_CHECK_INTERVAL: Duration = Duration::from_secs(5);

pub const ACME_CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

/// Where a tunnel's certificate is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertStatus {
    /// Loading a stored certificate, or waiting for the agent to connect before requesting one.
    Preparing,
    /// Fetching the challenge through the tunnel to make sure Let's Encrypt can reach it.
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
    /// Fetch each challenge through the tunnel before asking the ACME server to validate.
    self_check: bool,
    domains: Mutex<HashMap<String, DomainState>>,
    challenges: Mutex<HashMap<String, String>>,
    /// Set while the agent is connected, since challenges can only be answered then.
    active: AtomicBool,
    wake: Notify,
}

struct DomainState {
    status: CertStatus,
    acceptor: Option<TlsAcceptor>,
    expires_at: Option<u64>,
    loaded_from_disk: bool,
    failures: u32,
    next_attempt: u64,
}

impl DomainState {
    fn new() -> Self {
        DomainState {
            status: CertStatus::Preparing,
            acceptor: None,
            expires_at: None,
            loaded_from_disk: false,
            failures: 0,
            next_attempt: 0,
        }
    }

    fn needs_certificate(&self, now: u64) -> bool {
        match self.expires_at {
            Some(expires_at) => expires_at.saturating_sub(now) < RENEW_BEFORE_MS,
            None => true,
        }
    }
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

        Self::with_acme_server(store, directory_url, acme_root, true)
    }

    fn with_acme_server(
        store: CertStore,
        directory_url: String,
        acme_root: Option<std::path::PathBuf>,
        self_check: bool,
    ) -> Self {
        CertManager {
            inner: Arc::new(Inner {
                store,
                directory_url,
                acme_root,
                self_check,
                domains: Mutex::new(HashMap::new()),
                challenges: Mutex::new(HashMap::new()),
                active: AtomicBool::new(false),
                wake: Notify::new(),
            }),
        }
    }

    pub fn store(&self) -> &CertStore {
        &self.inner.store
    }

    /// Sets the domains that need certificates. Certificates for removed domains stay on
    /// disk so turning the option back on doesn't request a new one.
    pub fn set_domains<I: IntoIterator<Item = String>>(&self, domains: I) {
        let wanted: HashSet<String> = domains
            .into_iter()
            .map(|d| d.to_ascii_lowercase())
            .collect();
        let mut changed = false;
        {
            let mut current = self.inner.domains.lock().unwrap();
            current.retain(|domain, _| {
                let keep = wanted.contains(domain);
                changed |= !keep;
                keep
            });
            for domain in wanted {
                current.entry(domain).or_insert_with(|| {
                    changed = true;
                    DomainState::new()
                });
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
        let domains = self.inner.domains.lock().unwrap();
        domains
            .get(&domain.to_ascii_lowercase())
            .map(|state| state.status.clone())
    }

    pub fn acceptor(&self, domain: &str) -> Option<TlsAcceptor> {
        let domains = self.inner.domains.lock().unwrap();
        domains
            .get(&domain.to_ascii_lowercase())
            .and_then(|state| state.acceptor.clone())
    }

    /// The key authorization for a pending HTTP-01 challenge token.
    pub fn challenge_response(&self, token: &str) -> Option<String> {
        self.inner.challenges.lock().unwrap().get(token).cloned()
    }

    /// Loads stored certificates and requests new ones until `cancel` fires.
    pub async fn run(self, cancel: CancellationToken) {
        let mut account: Option<Account> = None;

        loop {
            self.load_stored().await;

            let now = now_milli();
            let next = self.next_domain_to_issue(now);
            if let Some(domain) = next {
                let result = tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = self.issue(&domain, &mut account) => result,
                };
                self.finish_attempt(&domain, result);
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
                Some(stored) => match build_acceptor(&stored) {
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
            if let Some((acceptor, expires_at)) = loaded {
                if expires_at > now_milli() {
                    tracing::info!(%domain, expires_at, "loaded stored TLS certificate");
                    state.acceptor = Some(acceptor);
                    state.expires_at = Some(expires_at);
                    state.status = CertStatus::Ready {
                        expires_at,
                        renewing: false,
                    };
                }
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

    fn next_wake_in(&self, now: u64) -> u64 {
        let domains = self.inner.domains.lock().unwrap();
        domains
            .values()
            .map(|state| {
                if state.needs_certificate(now) {
                    state.next_attempt.saturating_sub(now)
                } else {
                    state
                        .expires_at
                        .unwrap_or(now)
                        .saturating_sub(RENEW_BEFORE_MS)
                        .saturating_sub(now)
                }
            })
            .min()
            .unwrap_or(IDLE_CHECK_MS)
            .clamp(1_000, IDLE_CHECK_MS)
    }

    fn set_progress(&self, domain: &str, status: CertStatus) {
        let mut domains = self.inner.domains.lock().unwrap();
        let Some(state) = domains.get_mut(domain) else {
            return;
        };
        // A working certificate keeps serving while its replacement is requested.
        state.status = match (&state.status, state.acceptor.is_some()) {
            (CertStatus::Ready { expires_at, .. }, true) => CertStatus::Ready {
                expires_at: *expires_at,
                renewing: true,
            },
            _ => status,
        };
    }

    fn finish_attempt(&self, domain: &str, result: Result<(TlsAcceptor, u64), String>) {
        let now = now_milli();
        let mut domains = self.inner.domains.lock().unwrap();
        let Some(state) = domains.get_mut(domain) else {
            return;
        };

        match result {
            Ok((acceptor, expires_at)) => {
                tracing::info!(%domain, expires_at, "obtained TLS certificate from Let's Encrypt");
                state.acceptor = Some(acceptor);
                state.expires_at = Some(expires_at);
                state.failures = 0;
                state.next_attempt = 0;
                state.status = CertStatus::Ready {
                    expires_at,
                    renewing: false,
                };
            }
            Err(message) => {
                state.failures += 1;
                let index = (state.failures as usize - 1).min(RETRY_DELAYS_SECS.len() - 1);
                let retry_at = now + RETRY_DELAYS_SECS[index] * 1000;
                state.next_attempt = retry_at;
                tracing::error!(%domain, %message, retry_in_secs = RETRY_DELAYS_SECS[index], "failed to obtain TLS certificate");

                state.status = match (state.acceptor.is_some(), state.expires_at) {
                    (true, Some(expires_at)) if expires_at > now => CertStatus::Ready {
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

    fn account_builder(&self) -> Result<instant_acme::AccountBuilder, String> {
        match &self.inner.acme_root {
            Some(root) => Account::builder_with_root(root),
            None => Account::builder(),
        }
        .map_err(|e| format!("could not set up the ACME client: {e}"))
    }

    async fn account(&self, cached: &mut Option<Account>) -> Result<Account, String> {
        if let Some(account) = cached {
            return Ok(account.clone());
        }

        let directory = &self.inner.directory_url;
        if let Some(saved) = self.inner.store.load_account().await
            && let Ok(saved) = serde_json::from_str::<SavedAccount>(&saved)
            && &saved.directory == directory
        {
            let account = self
                .account_builder()?
                .from_credentials(saved.credentials)
                .await
                .map_err(|e| format!("could not load the Let's Encrypt account: {e}"))?;
            *cached = Some(account.clone());
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
            .map_err(|e| format!("could not create a Let's Encrypt account: {e}"))?;

        let saved = serde_json::to_string(&SavedAccount {
            directory: directory.clone(),
            credentials,
        })
        .map_err(|e| format!("could not serialize the Let's Encrypt account: {e}"))?;
        if let Err(error) = self.inner.store.save_account(&saved).await {
            tracing::warn!(%error, dir = %self.inner.store.dir().display(), "could not save the Let's Encrypt account");
        }

        *cached = Some(account.clone());
        Ok(account)
    }

    async fn issue(
        &self,
        domain: &str,
        account: &mut Option<Account>,
    ) -> Result<(TlsAcceptor, u64), String> {
        tracing::info!(%domain, "requesting TLS certificate from Let's Encrypt");
        self.set_progress(domain, CertStatus::Requesting);

        let mut tokens = ChallengeTokens {
            manager: self,
            tokens: Vec::new(),
        };
        let result = self.issue_inner(domain, account, &mut tokens).await;
        drop(tokens);

        if result.is_err() {
            // A failure may come from a stale account (e.g. deactivated); start fresh next time.
            *account = None;
        }
        result
    }

    async fn issue_inner(
        &self,
        domain: &str,
        account_cache: &mut Option<Account>,
        tokens: &mut ChallengeTokens<'_>,
    ) -> Result<(TlsAcceptor, u64), String> {
        let account = self.account(account_cache).await?;

        let identifiers = [Identifier::Dns(domain.to_string())];
        let mut order = account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(|e| format!("Let's Encrypt refused the certificate order: {e}"))?;

        let mut authorizations = order.authorizations();
        while let Some(authz) = authorizations.next().await {
            let mut authz = authz.map_err(|e| format!("could not load the authorization: {e}"))?;
            match authz.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                status => return Err(format!("the domain authorization is {status:?}")),
            }

            let mut challenge = authz
                .challenge(ChallengeType::Http01)
                .ok_or_else(|| "Let's Encrypt offered no HTTP challenge".to_string())?;
            let token = challenge.token.clone();
            let key_auth = challenge.key_authorization().as_str().to_string();
            tokens.add(token.clone(), key_auth.clone());

            self.set_progress(domain, CertStatus::CheckingReachability);
            if self.inner.self_check {
                self_check(domain, &token, &key_auth).await?;
            }

            self.set_progress(domain, CertStatus::Validating);
            challenge
                .set_ready()
                .await
                .map_err(|e| format!("could not start validation: {e}"))?;
        }

        let status = order
            .poll_ready(&RetryPolicy::new().timeout(Duration::from_secs(120)))
            .await
            .map_err(|e| format!("validation did not complete: {e}"))?;
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
            return Err(detail);
        }

        self.set_progress(domain, CertStatus::Issuing);
        let key_pem = order
            .finalize()
            .await
            .map_err(|e| format!("could not finalize the certificate: {e}"))?;
        let chain_pem = order
            .poll_certificate(&RetryPolicy::new().timeout(Duration::from_secs(60)))
            .await
            .map_err(|e| format!("could not download the certificate: {e}"))?;

        let stored = StoredCert { chain_pem, key_pem };
        let (acceptor, expires_at) = build_acceptor(&stored)?;
        if let Err(error) = self.inner.store.save_cert(domain, &stored).await {
            // The certificate still works for this run; it is requested again after a restart.
            tracing::error!(%domain, %error, dir = %self.inner.store.dir().display(), "could not save the TLS certificate");
        }
        Ok((acceptor, expires_at))
    }
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
}

impl Drop for ChallengeTokens<'_> {
    fn drop(&mut self) {
        let mut challenges = self.manager.inner.challenges.lock().unwrap();
        for token in &self.tokens {
            challenges.remove(token);
        }
    }
}

/// Fetches the challenge over the public internet, the way Let's Encrypt will.
async fn self_check(domain: &str, token: &str, key_auth: &str) -> Result<(), String> {
    let client = Client::builder(TokioExecutor::new()).build_http::<Empty<bytes::Bytes>>();
    let uri: hyper::Uri = format!("http://{domain}{ACME_CHALLENGE_PREFIX}{token}")
        .parse()
        .map_err(|e| format!("invalid challenge URL: {e}"))?;

    let mut last_error = String::new();
    for attempt in 1..=SELF_CHECK_ATTEMPTS {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let response = client
                .get(uri.clone())
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            let status = response.status();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|e| format!("reading the response failed: {e}"))?
                .to_bytes();
            if status.is_success() && body.as_ref() == key_auth.as_bytes() {
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
                tracing::debug!(%domain, attempt, %error, "challenge self-check failed");
                last_error = error;
            }
        }
        tokio::time::sleep(SELF_CHECK_INTERVAL).await;
    }

    Err(format!(
        "http://{domain}{ACME_CHALLENGE_PREFIX} isn't reachable through the tunnel ({last_error})"
    ))
}

/// Builds a TLS acceptor from a stored certificate and returns it with the expiry time
/// (milliseconds since the Unix epoch) of the leaf certificate.
pub fn build_acceptor(stored: &StoredCert) -> Result<(TlsAcceptor, u64), String> {
    let chain = CertificateDer::pem_slice_iter(stored.chain_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid certificate PEM: {e}"))?;
    let leaf = chain
        .first()
        .ok_or_else(|| "the certificate file is empty".to_string())?;
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|e| format!("invalid certificate: {e}"))?;
    let expires_at = (parsed.validity().not_after.timestamp().max(0) as u64) * 1000;

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

    Ok((TlsAcceptor::from(Arc::new(config)), expires_at))
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

    fn self_signed(domain: &str) -> StoredCert {
        let cert = rcgen::generate_simple_self_signed(vec![domain.to_string()]).unwrap();
        StoredCert {
            chain_pem: cert.cert.pem(),
            key_pem: cert.signing_key.serialize_pem(),
        }
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

    #[tokio::test]
    async fn stored_certificate_is_loaded_without_requesting() {
        let dir = std::env::temp_dir().join(format!(
            "playit-cert-manager-{}-{}",
            std::process::id(),
            now_milli()
        ));
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
        let manager = CertManager::new(CertStore::new(std::env::temp_dir().join("unused")));
        manager.set_domains(["site.example.com".to_string()]);
        manager
            .inner
            .domains
            .lock()
            .unwrap()
            .get_mut("site.example.com")
            .unwrap()
            .loaded_from_disk = true;

        assert_eq!(manager.next_domain_to_issue(now_milli()), None);
        manager.set_active(true);
        assert_eq!(
            manager.next_domain_to_issue(now_milli()).as_deref(),
            Some("site.example.com")
        );
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

        let dir = std::env::temp_dir().join(format!(
            "playit-pebble-{}-{}",
            std::process::id(),
            now_milli()
        ));
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
                false,
            )
        };
        let manager = new_manager();
        manager.set_domains(["site.example.com".to_string()]);
        manager.load_stored().await;

        let mut account = None;
        let mut result = Err(String::new());
        // Pebble needs a moment to start listening.
        for _ in 0..20 {
            result = manager.issue("site.example.com", &mut account).await;
            if result.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let (_, expires_at) = result.expect("pebble issued a certificate");
        assert!(expires_at > now_milli());
        assert!(manager.inner.challenges.lock().unwrap().is_empty());

        // A new manager (as after a restart) loads the stored certificate and account.
        let restarted = new_manager();
        restarted.set_domains(["site.example.com".to_string()]);
        restarted.load_stored().await;
        assert!(restarted.acceptor("site.example.com").is_some());
        let mut account = None;
        restarted
            .account(&mut account)
            .await
            .expect("stored account loads");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failures_back_off_and_keep_a_valid_certificate() {
        let manager = CertManager::new(CertStore::new(std::env::temp_dir().join("unused")));
        manager.set_domains(["site.example.com".to_string()]);

        manager.finish_attempt("site.example.com", Err("boom".into()));
        match manager.status("site.example.com").unwrap() {
            CertStatus::Failed { message, retry_at } => {
                assert_eq!(message, "boom");
                assert!(retry_at >= now_milli() + 59_000);
            }
            other => panic!("unexpected status {other:?}"),
        }

        let (acceptor, expires_at) = build_acceptor(&self_signed("site.example.com")).unwrap();
        {
            let mut domains = manager.inner.domains.lock().unwrap();
            let state = domains.get_mut("site.example.com").unwrap();
            state.acceptor = Some(acceptor);
            state.expires_at = Some(expires_at);
        }
        manager.finish_attempt("site.example.com", Err("renewal failed".into()));
        assert!(manager.status("site.example.com").unwrap().is_ready());
        assert!(manager.acceptor("site.example.com").is_some());
    }
}
