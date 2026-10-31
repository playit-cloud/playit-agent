//! Host side of NetherNet External Signaling (NXS): registers the bridge with playit's
//! signaling service using the tunnel's NetherNet token, then heartbeats the bridge's WebRTC
//! profile so the service can answer player offers on its behalf.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P384_SHA384_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::admission::{AdmissionKey, AdmissionKeys};

pub const PROVIDER_ORIGIN: &str = "https://nethernet.playit.gg";
const PROTOCOL: &str = "nethernet-external-signaling-v1";
const SIGNATURE_VERSION: &str = "nxs-es384-v1";
const DISCOVERY_PATH: &str = "/.well-known/nethernet-external-signaling";
const STATELESS_CAPABILITY: &str = "nethernet.stateless-admission.v1";

const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const ATTEMPTS_PER_REQUEST: u32 = 3;

/// A tunnel's host registration token: `pnx1.<tunnel id>.<base64url secret>`.
#[derive(Clone, PartialEq, Eq)]
pub struct NxsToken(String);

impl NxsToken {
    pub fn parse(token: &str) -> Option<Self> {
        let mut parts = token.trim().split('.');
        let valid = parts.next() == Some("pnx1")
            && parts.next().is_some_and(|id| id.parse::<u64>().is_ok())
            && parts.next().is_some_and(|secret| URL_SAFE_NO_PAD.decode(secret).is_ok())
            && parts.next().is_none();
        valid.then(|| NxsToken(token.trim().to_string()))
    }

    pub fn tunnel_id(&self) -> u64 {
        self.0.split('.').nth(1).and_then(|id| id.parse().ok()).unwrap_or(0)
    }
}

impl std::fmt::Debug for NxsToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NxsToken(tunnel {})", self.tunnel_id())
    }
}

/// What the bridge tells the provider about itself on each heartbeat.
#[derive(Clone, Debug)]
pub struct HostProfile {
    /// `sha-256 AA:BB:...` of the bridge's DTLS certificate.
    pub dtls_fingerprint: String,
    /// Random per start; tokens sealed for an older incarnation do not open.
    pub incarnation: String,
    pub candidate: std::net::SocketAddr,
    pub sctp_port: u16,
    pub max_message_size: u32,
}

/// The Bedrock server's state, as its RakNet status reply reports it.
#[derive(Clone, Debug, Default)]
pub struct ServerSample {
    pub reachable: bool,
    pub name: String,
    pub level: String,
    pub players: u64,
    pub max_players: u64,
    pub game_type: u8,
    pub protocol: Option<u32>,
    pub version: String,
    pub sampled_at_ms: u64,
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct SavedKey {
    key_id: String,
    secret: String,
    #[serde(default)]
    retire_after: Option<u64>,
}

/// Saved across restarts so the bridge recovers its registration instead of enrolling anew:
/// the provider caps registrations per tunnel and never expires them.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct SavedState {
    origin: String,
    tunnel_id: u64,
    /// base64url PKCS#8 of the P-384 machine key.
    machine_key: String,
    #[serde(default)]
    challenge_id: Option<String>,
    #[serde(default)]
    registration_id: Option<String>,
    #[serde(default)]
    instance_id: Option<String>,
    #[serde(default)]
    key_id: Option<String>,
    #[serde(default)]
    admission_keys: Vec<SavedKey>,
}

struct StateFile {
    path: Option<PathBuf>,
    state: SavedState,
}

impl StateFile {
    fn load(path: Option<PathBuf>, tunnel_id: u64) -> Self {
        let state = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|data| serde_json::from_slice::<SavedState>(&data).ok())
            .filter(|state| state.origin == PROVIDER_ORIGIN && state.tunnel_id == tunnel_id)
            .unwrap_or_default();
        StateFile { path, state }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(data) = serde_json::to_vec_pretty(&self.state) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        let result = write_private(&tmp, &data).and_then(|_| std::fs::rename(&tmp, path));
        if let Err(error) = result {
            tracing::error!(?error, path = %path.display(), "failed to save NetherNet registration state");
        }
    }
}

fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

struct MachineKey {
    pair: EcdsaKeyPair,
    jwk: Value,
    thumbprint: String,
}

impl MachineKey {
    fn load_or_create(state: &mut StateFile, rng: &SystemRandom) -> Self {
        if let Some(key) = URL_SAFE_NO_PAD
            .decode(&state.state.machine_key)
            .ok()
            .and_then(|pkcs8| EcdsaKeyPair::from_pkcs8(&ECDSA_P384_SHA384_FIXED_SIGNING, &pkcs8, rng).ok())
        {
            return Self::from_pair(key);
        }

        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P384_SHA384_FIXED_SIGNING, rng).expect("generate P-384 key");
        /* A new key cannot recover the old registration. */
        state.state = SavedState {
            origin: PROVIDER_ORIGIN.to_string(),
            tunnel_id: state.state.tunnel_id,
            machine_key: URL_SAFE_NO_PAD.encode(pkcs8.as_ref()),
            ..SavedState::default()
        };
        state.save();
        Self::from_pair(EcdsaKeyPair::from_pkcs8(&ECDSA_P384_SHA384_FIXED_SIGNING, pkcs8.as_ref(), rng).unwrap())
    }

    fn from_pair(pair: EcdsaKeyPair) -> Self {
        let point = pair.public_key().as_ref();
        let x = URL_SAFE_NO_PAD.encode(&point[1..49]);
        let y = URL_SAFE_NO_PAD.encode(&point[49..97]);
        let thumbprint = jwk_thumbprint(&x, &y);
        MachineKey {
            pair,
            jwk: json!({"kty": "EC", "crv": "P-384", "x": x, "y": y}),
            thumbprint,
        }
    }

    fn sign(&self, rng: &SystemRandom, payload: &str) -> String {
        let signature = self.pair.sign(rng, payload.as_bytes()).expect("ecdsa sign");
        URL_SAFE_NO_PAD.encode(signature.as_ref())
    }
}

/// RFC 7638 thumbprint of a P-384 public key.
fn jwk_thumbprint(x: &str, y: &str) -> String {
    let canonical = format!(r#"{{"crv":"P-384","kty":"EC","x":"{x}","y":"{y}"}}"#);
    URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, canonical.as_bytes()))
}

/// The provider signs and verifies JSON arrays exactly as `JSON.stringify` writes them.
fn canonical(items: &[Value]) -> String {
    serde_json::to_string(items).expect("serialize canonical array")
}

fn sha256_b64(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, data))
}

fn random_id(rng: &SystemRandom) -> String {
    let mut bytes = [0u8; 18];
    ring::rand::SecureRandom::fill(rng, &mut bytes).expect("random");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now_ms() -> u64 {
    crate::utils::now_milli()
}

#[derive(Debug)]
enum NxsError {
    Transport(reqwest::Error),
    /// The provider refused the request with this error code.
    Refused {
        status: u16,
        code: String,
        message: Option<String>,
        retry_after: Option<Duration>,
    },
    Invalid(&'static str),
}

impl NxsError {
    fn code(&self) -> Option<&str> {
        match self {
            NxsError::Refused { code, .. } => Some(code),
            _ => None,
        }
    }

    fn is_retryable(&self) -> bool {
        match self {
            NxsError::Transport(_) => true,
            NxsError::Refused { status, .. } => matches!(status, 429 | 502 | 503 | 504),
            NxsError::Invalid(_) => false,
        }
    }

    fn retry_after(&self) -> Option<Duration> {
        match self {
            NxsError::Refused { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

impl std::fmt::Display for NxsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NxsError::Transport(error) => write!(f, "{error}"),
            NxsError::Refused { status, code, message, .. } => {
                write!(f, "{status} {code}")?;
                if let Some(message) = message {
                    write!(f, ": {message}")?;
                }
                Ok(())
            }
            NxsError::Invalid(what) => write!(f, "invalid provider response: {what}"),
        }
    }
}

struct Registration {
    instance_id: String,
    key_id: String,
    generation: u64,
    sequence: u64,
}

/// Runs registration and heartbeats until cancelled, publishing the admission keys that
/// player tokens open under.
pub struct NxsHost {
    http: reqwest::Client,
    origin: String,
    rng: SystemRandom,
    token: NxsToken,
    state: StateFile,
    key: MachineKey,
    keys_tx: watch::Sender<Arc<AdmissionKeys>>,
    profile: HostProfile,
    pending_key_request: Option<String>,
}

impl NxsHost {
    pub fn new(
        token: NxsToken,
        state_path: Option<PathBuf>,
        profile: HostProfile,
        keys_tx: watch::Sender<Arc<AdmissionKeys>>,
    ) -> Self {
        let rng = SystemRandom::new();
        let mut state = StateFile::load(state_path, token.tunnel_id());
        state.state.tunnel_id = token.tunnel_id();
        let key = MachineKey::load_or_create(&mut state, &rng);
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("build http client");

        NxsHost {
            http,
            origin: PROVIDER_ORIGIN.to_string(),
            rng,
            token,
            state,
            key,
            keys_tx,
            profile,
            pending_key_request: None,
        }
    }

    pub async fn run<F, Fut>(mut self, cancel: CancellationToken, sample: F)
    where
        F: Fn() -> Fut,
        Fut: Future<Output = ServerSample>,
    {
        let mut backoff = DEFAULT_HEARTBEAT;
        loop {
            let registration = tokio::select! {
                _ = cancel.cancelled() => return,
                result = self.register() => result,
            };
            let mut registration = match registration {
                Ok(registration) => {
                    backoff = DEFAULT_HEARTBEAT;
                    registration
                }
                Err(error) => {
                    let wait = error.retry_after().unwrap_or(backoff).max(backoff);
                    tracing::error!(%error, retry_in_secs = wait.as_secs(), tunnel_id = self.token.tunnel_id(), "NetherNet signaling registration failed");
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tokio::time::sleep(wait) => continue,
                    }
                }
            };
            self.publish_keys();
            tracing::info!(tunnel_id = self.token.tunnel_id(), "registered with NetherNet signaling");

            let mut was_routable = None;
            let mut interval;
            let reregister = loop {
                let started = tokio::time::Instant::now();
                let server = sample().await;
                match self.heartbeat(&mut registration, &server, true).await {
                    Ok(beat) => {
                        backoff = DEFAULT_HEARTBEAT;
                        interval = beat.next;
                        if was_routable != Some(beat.routable) {
                            if beat.routable {
                                tracing::info!(tunnel_id = self.token.tunnel_id(), "NetherNet tunnel is accepting players");
                            } else {
                                tracing::warn!(tunnel_id = self.token.tunnel_id(), reasons = ?beat.reasons, "NetherNet signaling is not routing players to this agent");
                            }
                            was_routable = Some(beat.routable);
                        }
                        if beat.again_soon {
                            interval = Duration::from_secs(2);
                        }
                    }
                    Err(error) => match error.code() {
                        Some("recovery_required" | "unknown_generation") => break true,
                        Some("generation_fenced" | "registration_gone") => {
                            tracing::error!(%error, tunnel_id = self.token.tunnel_id(), "another host took over this NetherNet registration");
                            self.state.state.registration_id = None;
                            self.state.save();
                            break true;
                        }
                        Some("unknown_admission_key") => {
                            self.state.state.admission_keys.clear();
                            self.state.save();
                            interval = Duration::from_secs(2);
                        }
                        _ => {
                            interval = error.retry_after().unwrap_or(backoff).max(backoff);
                            backoff = (backoff * 2).min(MAX_BACKOFF);
                            tracing::warn!(%error, retry_in_secs = interval.as_secs(), "NetherNet signaling heartbeat failed");
                        }
                    },
                }

                tokio::select! {
                    _ = cancel.cancelled() => break false,
                    _ = tokio::time::sleep_until(started + interval) => {}
                }
            };

            if !reregister {
                /* Stop routing players here before the agent goes away. */
                let mut server = sample().await;
                server.reachable = false;
                let _ = tokio::time::timeout(Duration::from_secs(3), self.heartbeat(&mut registration, &server, false)).await;
                return;
            }
        }
    }

    fn publish_keys(&self) {
        let now = now_ms();
        let keys = self
            .state
            .state
            .admission_keys
            .iter()
            .filter(|key| key.retire_after.is_none_or(|at| now < at))
            .map(|key| AdmissionKey {
                key_id: key.key_id.clone(),
                secret: key.secret.clone(),
                retire_after: key.retire_after,
            })
            .collect();
        let _ = self.keys_tx.send(Arc::new(AdmissionKeys::new(&self.profile.incarnation, keys)));
    }

    async fn discover(&self) -> Result<Duration, NxsError> {
        let response = self
            .http
            .get(format!("{}{DISCOVERY_PATH}", self.origin))
            .send()
            .await
            .map_err(NxsError::Transport)?;
        let body: Value = Self::read_json(response).await?;
        let lists = |key: &str, value: &str| {
            body[key].as_array().is_some_and(|items| items.iter().any(|item| item == value))
        };
        if body["provider"] != self.origin.as_str() || !lists("protocols", PROTOCOL) || !lists("signatures", SIGNATURE_VERSION) {
            return Err(NxsError::Invalid("discovery does not offer nethernet-external-signaling-v1"));
        }
        Ok(body["limits"]["heartbeatIntervalMs"]
            .as_u64()
            .filter(|ms| (1000..=30_000).contains(ms))
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_HEARTBEAT))
    }

    /// Recovers the saved registration, or enrolls with the tunnel token when there is none.
    async fn register(&mut self) -> Result<Registration, NxsError> {
        self.discover().await?;

        if let Some(registration_id) = self.state.state.registration_id.clone() {
            match self.recover(&registration_id).await {
                Ok(registration) => return Ok(registration),
                Err(error) if error.code() == Some("recovery_unavailable") => {
                    tracing::warn!("saved NetherNet registration is gone; enrolling again");
                    self.state.state.registration_id = None;
                    self.state.state.instance_id = None;
                    self.state.state.key_id = None;
                    self.state.state.admission_keys.clear();
                    self.state.save();
                }
                Err(error) => return Err(error),
            }
        }

        let challenge = self
            .post_json(
                "/v1/nxs/register",
                &json!({
                    "protocol": PROTOCOL,
                    "publicKeyJwk": self.key.jwk,
                    "label": "playit agent",
                }),
                Some(&self.token.0),
            )
            .await?;
        self.check_challenge(&challenge, "attach-instance")?;
        self.state.state.challenge_id = challenge["challengeId"].as_str().map(str::to_string);
        self.state.save();

        let registration = match self.complete(&challenge).await {
            Err(error) if error.code() == Some("recovery_required") => {
                /* The completion went through but its reply was lost. */
                let challenge_id = self.state.state.challenge_id.clone().unwrap_or_default();
                return self.recover_by("challengeId", &challenge_id).await;
            }
            other => other?,
        };
        Ok(registration)
    }

    async fn recover(&mut self, registration_id: &str) -> Result<Registration, NxsError> {
        self.recover_by("registrationId", registration_id).await
    }

    async fn recover_by(&mut self, field: &str, id: &str) -> Result<Registration, NxsError> {
        let challenge = self
            .post_json("/v1/nxs/register", &json!({"protocol": PROTOCOL, field: id}), None)
            .await?;
        self.check_challenge(&challenge, "recover")?;
        self.complete(&challenge).await
    }

    fn check_challenge(&self, challenge: &Value, mode: &str) -> Result<(), NxsError> {
        let context = &challenge["context"];
        let digest = sha256_b64(
            canonical(&[
                context["mode"].clone(),
                context["label"].clone(),
                context["authorizationId"].clone(),
                context["serviceId"].clone(),
                context["registrationId"].clone(),
            ])
            .as_bytes(),
        );
        let valid = challenge["audience"] == self.origin.as_str()
            && challenge["thumbprint"] == self.key.thumbprint.as_str()
            && challenge["contextDigest"] == digest.as_str()
            && context["mode"] == mode
            && challenge["pow"]["difficulty"].as_u64().unwrap_or(0) == 0
            && challenge["expiresAt"].as_u64().is_some_and(|at| now_ms() < at);
        if valid { Ok(()) } else { Err(NxsError::Invalid("challenge does not match this host")) }
    }

    async fn complete(&mut self, challenge: &Value) -> Result<Registration, NxsError> {
        let proof_nonce = "0";
        let idempotency_key = random_id(&self.rng);
        let payload = canonical(&[
            json!(PROTOCOL),
            json!("complete"),
            challenge["audience"].clone(),
            challenge["challengeId"].clone(),
            challenge["nonce"].clone(),
            challenge["thumbprint"].clone(),
            challenge["contextDigest"].clone(),
            challenge["expiresAt"].clone(),
            json!(proof_nonce),
            json!(idempotency_key),
        ]);
        let request = json!({
            "protocol": PROTOCOL,
            "challengeId": challenge["challengeId"],
            "proofNonce": proof_nonce,
            "idempotencyKey": idempotency_key,
            "signature": self.key.sign(&self.rng, &payload),
        });

        let mut last_error = None;
        for attempt in 0..ATTEMPTS_PER_REQUEST * 2 {
            match self.post_json("/v1/nxs/complete", &request, None).await {
                Ok(reply) => return self.accept_registration(&reply),
                Err(error) if error.is_retryable() => {
                    let wait = error.retry_after().unwrap_or(Duration::from_millis(250 << attempt.min(4)));
                    last_error = Some(error);
                    tokio::time::sleep(wait.min(Duration::from_secs(10))).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap())
    }

    fn accept_registration(&mut self, reply: &Value) -> Result<Registration, NxsError> {
        let field = |name: &str| reply[name].as_str().map(str::to_string).ok_or(NxsError::Invalid("registration is missing ids"));
        let registration_id = field("registrationId")?;
        let instance_id = field("instanceId")?;
        let key_id = field("keyId")?;
        let generation = reply["leaseGeneration"].as_u64().ok_or(NxsError::Invalid("registration is missing leaseGeneration"))?;

        let saved = &mut self.state.state;
        saved.challenge_id = None;
        saved.registration_id = Some(registration_id);
        saved.instance_id = Some(instance_id.clone());
        saved.key_id = Some(key_id.clone());
        if let Some(key) = Self::read_admission_key(&reply["admissionKey"]) {
            saved.admission_keys = vec![key];
        }
        self.state.save();

        Ok(Registration {
            instance_id,
            key_id,
            generation,
            sequence: 0,
        })
    }

    fn read_admission_key(value: &Value) -> Option<SavedKey> {
        let key_id = value["keyId"].as_str()?;
        let secret = value["secret"].as_str()?;
        let valid_id = key_id.len() == 4 && key_id.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        (valid_id && secret.len() == 43).then(|| SavedKey {
            key_id: key_id.to_string(),
            secret: secret.to_string(),
            retire_after: None,
        })
    }

    async fn heartbeat(
        &mut self,
        registration: &mut Registration,
        server: &ServerSample,
        accepting: bool,
    ) -> Result<Heartbeat, NxsError> {
        let now = now_ms();
        self.state.state.admission_keys.retain(|key| key.retire_after.is_none_or(|at| now < at));

        let accepting = accepting && server.reachable;
        let max_players = server.max_players.clamp(1, 1_000_000);
        let mut body = json!({
            "acceptingPlayers": accepting,
            "capacity": if accepting { max_players } else { 0 },
            "connectedPlayers": {"count": server.players.min(1_000_000), "sampledAt": server.sampled_at_ms.min(now)},
            "software": {"name": "playit", "version": env!("CARGO_PKG_VERSION")},
        });
        if server.reachable && !server.name.trim().is_empty() {
            body["serverStatus"] = json!({
                "name": server.name.chars().filter(|c| !c.is_control()).collect::<String>(),
                "level": server.level.chars().filter(|c| !c.is_control()).collect::<String>(),
                "maxPlayerCount": max_players,
                "gameType": server.game_type.min(2),
            });
        }
        if let Some(protocol) = server.protocol {
            body["bedrockProtocols"] = json!({"accepted": [protocol], "version": server.version});
        }

        let installed: Vec<&str> = self.state.state.admission_keys.iter().map(|key| key.key_id.as_str()).collect();
        if let Some(active) = installed.last() {
            body["installedAdmissionKeyIds"] = json!(installed);
            body["hostProfile"] = json!({
                "candidates": [{
                    "foundation": "1",
                    "priority": 2130706431u64,
                    "address": self.profile.candidate.ip().to_string(),
                    "port": self.profile.candidate.port(),
                    "type": "host",
                }],
                "dtlsFingerprint": self.profile.dtls_fingerprint,
                "admissionKeyId": active,
                "sctpPort": self.profile.sctp_port,
                "maxMessageSize": self.profile.max_message_size,
                "statelessAdmission": {"capability": STATELESS_CAPABILITY, "incarnation": self.profile.incarnation},
            });
        }
        if installed.is_empty() {
            let request_id = self.pending_key_request.get_or_insert_with(|| random_id(&self.rng)).clone();
            body["admissionKeyRequestId"] = json!(request_id);
        }

        registration.sequence += 1;
        let reply = self.signed_post("/v1/nxs/heartbeat", registration, &body).await?;

        let mut again_soon = false;
        if let Some(key) = Self::read_admission_key(&reply["admissionKey"]) {
            let requested = reply["admissionKeyRequest"]["id"].as_str() == self.pending_key_request.as_deref();
            if requested || self.pending_key_request.is_none() {
                self.pending_key_request = None;
                self.state.state.admission_keys.retain(|saved| saved.key_id != key.key_id);
                self.state.state.admission_keys.push(key);
                self.state.save();
                self.publish_keys();
                again_soon = true;
            }
        } else if reply["admissionKeyRequest"].is_object() && reply["admissionKey"].is_null() {
            /* The secret was delivered to a reply we never saw; ask again under a new id. */
            self.pending_key_request = None;
            again_soon = true;
        }
        if let Some(retirements) = reply["admissionKeyRetirements"].as_array() {
            for retirement in retirements {
                let (Some(key_id), Some(at)) = (retirement["keyId"].as_str(), retirement["retireAfter"].as_u64()) else {
                    continue;
                };
                if let Some(saved) = self.state.state.admission_keys.iter_mut().find(|key| key.key_id == key_id) {
                    saved.retire_after = Some(saved.retire_after.map_or(at, |old| old.min(at)));
                }
            }
            self.state.save();
            self.publish_keys();
        }

        let routable = reply["readiness"]["routable"].as_bool().unwrap_or(false);
        let reasons = reply["readiness"]["reasons"]
            .as_array()
            .map(|reasons| reasons.iter().filter_map(|r| r.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let next = reply["checkIn"]["afterMillis"]
            .as_u64()
            .filter(|ms| (1000..=60_000).contains(ms))
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_HEARTBEAT);

        Ok(Heartbeat {
            routable,
            reasons,
            next,
            again_soon,
        })
    }

    /// Sends a signed operation, retrying transient failures with the same sequence and body.
    async fn signed_post(&self, path: &str, registration: &Registration, body: &Value) -> Result<Value, NxsError> {
        let body = serde_json::to_vec(body).expect("serialize body");
        let idempotency_key = random_id(&self.rng);

        let mut attempt = 0;
        loop {
            let timestamp = now_ms();
            let payload = canonical(&[
                json!(PROTOCOL),
                json!(SIGNATURE_VERSION),
                json!(self.origin),
                json!("POST"),
                json!(path),
                json!(timestamp),
                json!(registration.instance_id),
                json!(registration.key_id),
                json!(idempotency_key),
                json!(registration.generation),
                json!(registration.sequence),
                json!(sha256_b64(&body)),
            ]);
            let request = self
                .http
                .post(format!("{}{path}", self.origin))
                .header("content-type", "application/json")
                .header("nxs-instance-id", &registration.instance_id)
                .header("nxs-key-id", &registration.key_id)
                .header("nxs-timestamp", timestamp.to_string())
                .header("nxs-signature-version", SIGNATURE_VERSION)
                .header("nxs-generation", registration.generation.to_string())
                .header("nxs-sequence", registration.sequence.to_string())
                .header("idempotency-key", &idempotency_key)
                .header("nxs-signature", self.key.sign(&self.rng, &payload))
                .body(body.clone());

            let result = match request.send().await {
                Ok(response) => Self::read_json(response).await,
                Err(error) => Err(NxsError::Transport(error)),
            };
            match result {
                Err(error) if error.is_retryable() && attempt + 1 < ATTEMPTS_PER_REQUEST => {
                    let wait = error.retry_after().filter(|wait| *wait <= Duration::from_secs(10));
                    tokio::time::sleep(wait.unwrap_or(Duration::from_millis(250 << attempt))).await;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn post_json(&self, path: &str, body: &Value, bearer: Option<&str>) -> Result<Value, NxsError> {
        let mut attempt = 0;
        loop {
            let mut request = self
                .http
                .post(format!("{}{path}", self.origin))
                .header("content-type", "application/json")
                .body(serde_json::to_vec(body).expect("serialize body"));
            if let Some(bearer) = bearer {
                request = request.bearer_auth(bearer);
            }
            let result = match request.send().await {
                Ok(response) => Self::read_json(response).await,
                Err(error) => Err(NxsError::Transport(error)),
            };
            match result {
                /* complete() retries itself; a replayed completion has different semantics. */
                Err(error) if error.is_retryable() && attempt + 1 < ATTEMPTS_PER_REQUEST && path != "/v1/nxs/complete" => {
                    let wait = error.retry_after().filter(|wait| *wait <= Duration::from_secs(10));
                    tokio::time::sleep(wait.unwrap_or(Duration::from_millis(250 << attempt))).await;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn read_json(response: reqwest::Response) -> Result<Value, NxsError> {
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        Err(NxsError::Refused {
            status: status.as_u16(),
            code: body["code"].as_str().unwrap_or("unknown").to_string(),
            message: body["message"].as_str().map(str::to_string),
            retry_after,
        })
    }
}

struct Heartbeat {
    routable: bool,
    reasons: Vec<String>,
    next: Duration,
    again_soon: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("test_fixtures/nxs-v1.fixtures.json")).unwrap()
    }

    #[test]
    fn thumbprint_matches_fixture() {
        let fixture = fixture();
        let jwk = &fixture["publicKeyJwk"];
        assert_eq!(
            jwk_thumbprint(jwk["x"].as_str().unwrap(), jwk["y"].as_str().unwrap()),
            fixture["challenge"]["thumbprint"].as_str().unwrap()
        );
    }

    #[test]
    fn canonical_payloads_match_fixture() {
        let fixture = fixture();
        let challenge = &fixture["challenge"];
        let context = &challenge["context"];
        let digest = sha256_b64(
            canonical(&[
                context["mode"].clone(),
                context["label"].clone(),
                context["authorizationId"].clone(),
                context["serviceId"].clone(),
                context["registrationId"].clone(),
            ])
            .as_bytes(),
        );
        assert_eq!(digest, challenge["contextDigest"].as_str().unwrap());

        let proof = canonical(&[
            json!(PROTOCOL),
            json!("complete"),
            challenge["audience"].clone(),
            challenge["challengeId"].clone(),
            challenge["nonce"].clone(),
            challenge["thumbprint"].clone(),
            challenge["contextDigest"].clone(),
            challenge["expiresAt"].clone(),
            fixture["proofNonce"].clone(),
            fixture["idempotencyKey"].clone(),
        ]);
        assert_eq!(proof, fixture["proof"]["payload"].as_str().unwrap());

        let input = &fixture["request"]["input"];
        let request = canonical(&[
            json!(PROTOCOL),
            json!(SIGNATURE_VERSION),
            input["audience"].clone(),
            input["method"].clone(),
            input["path"].clone(),
            input["timestamp"].clone(),
            input["instanceId"].clone(),
            input["keyId"].clone(),
            input["idempotencyKey"].clone(),
            input["generation"].clone(),
            input["sequence"].clone(),
            json!(sha256_b64(input["body"].as_str().unwrap().as_bytes())),
        ]);
        assert_eq!(request, fixture["request"]["payload"].as_str().unwrap());
    }

    #[test]
    fn token_parsing() {
        let token = NxsToken::parse("pnx1.42.AAECAwQ").unwrap();
        assert_eq!(token.tunnel_id(), 42);
        assert!(NxsToken::parse("pnx1.x.AAECAwQ").is_none());
        assert!(NxsToken::parse("pnx2.42.AAECAwQ").is_none());
        assert!(!format!("{token:?}").contains("AAECAwQ"));
    }
}
