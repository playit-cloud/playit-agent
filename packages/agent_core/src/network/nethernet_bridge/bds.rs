//! The Bedrock Dedicated Server end of the bridge: its NetherNet signaling (`transport=nethernet`).
//!
//! The server answers HTTP on its `server-port`: `GET /v1/join` returns status JSON and
//! `POST /v1/join/<network id>` takes an SDP offer and returns the answer. The offer must carry
//! an `a=identity` whose key signs the offer's DTLS fingerprints. The server checks that
//! signature but not who issued the token, so the bridge signs with its own key. The player's
//! Xbox identity reaches the server later, inside the Login packet the bridge passes through.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P384_SHA384_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde_json::{Value, json};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// What the server sends when it gathered no UDP port for the session.
const DISCARD_PORT: u16 = 9;
const IDENTITY_DOMAIN: &str = "self";
const IDENTITY_LIFETIME_SECS: u64 = 86_400;

/// DER prefix of a named-curve secp384r1 SubjectPublicKeyInfo with an uncompressed point.
const P384_SPKI_PREFIX: [u8; 23] = [
    0x30, 0x76, 0x30, 0x10, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x05, 0x2b,
    0x81, 0x04, 0x00, 0x22, 0x03, 0x62, 0x00,
];

#[derive(Debug, Default, Clone)]
pub struct ServerStatus {
    pub name: String,
    pub level: String,
    pub players: u64,
    pub max_players: u64,
    pub game_type: u8,
    pub protocol: Option<u32>,
    pub version: String,
}

impl ServerStatus {
    /// Reads the status JSON. The server sends an empty body when `enable-lan-visibility=false`.
    pub fn parse(body: &[u8]) -> ServerStatus {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return ServerStatus::default();
        };
        let text = |key: &str| value[key].as_str().unwrap_or_default().to_string();
        let number = |key: &str| value[key].as_u64();
        ServerStatus {
            name: text("name"),
            level: text("level"),
            players: number("players").unwrap_or(0),
            max_players: number("maxPlayers").unwrap_or(0),
            game_type: number("gameType").unwrap_or(0) as u8,
            protocol: number("protocol")
                .filter(|&protocol| protocol != 0)
                .map(|protocol| protocol as u32),
            version: text("version"),
        }
    }
}

#[derive(Debug)]
pub enum SignalingError {
    Http(String),
    /// The server replied without an answer. Its body is a numeric error code.
    Refused(String),
    /// The answer names no UDP port to reach the server on.
    NoPort,
}

pub struct Signaling {
    http: reqwest::Client,
}

impl Default for Signaling {
    fn default() -> Self {
        Signaling {
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("build http client"),
        }
    }
}

impl Signaling {
    /// `None` when the server does not answer at all.
    pub async fn status(&self, server: SocketAddr, timeout: Duration) -> Option<ServerStatus> {
        let response = self
            .http
            .get(format!("http://{server}/v1/join"))
            .timeout(timeout)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        Some(ServerStatus::parse(&response.bytes().await.ok()?))
    }

    /// Sends an offer and returns the server's answer.
    pub async fn offer(&self, server: SocketAddr, offer: &str) -> Result<String, SignalingError> {
        let network_id: u64 = rand::random();
        let response = self
            .http
            .post(format!("http://{server}/v1/join/{network_id}"))
            .header(reqwest::header::CONTENT_TYPE, "application/sdp")
            .body(offer.to_string())
            .send()
            .await
            .map_err(|error| SignalingError::Http(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| SignalingError::Http(error.to_string()))?;
        if !status.is_success() {
            return Err(SignalingError::Http(status.to_string()));
        }
        /* Refusals are a 200 too, with a short error code instead of SDP. */
        if !body.starts_with("v=0") {
            return Err(SignalingError::Refused(body.chars().take(64).collect()));
        }
        Ok(body)
    }
}

pub struct Answer {
    /// The answer as str0m takes it.
    pub sdp: String,
    /// Where the bridge sends the session's UDP.
    pub target: SocketAddr,
}

/// Prepares the server's answer for str0m: drops `a=identity`, which str0m does not read, and
/// the candidates, which pick the address the bridge sends to instead.
///
/// The server binds its UDP only on the interfaces it lists, not necessarily the one its
/// signaling answered on (on Windows, never on loopback). `server` is the signaling address.
pub fn prepare_answer(answer: &str, server: SocketAddr) -> Result<Answer, SignalingError> {
    let mut port = None;
    let mut candidates = Vec::new();
    let mut sdp = String::with_capacity(answer.len());
    for line in answer.lines() {
        if line.starts_with("a=identity:") {
            continue;
        }
        if let Some(candidate) = line.strip_prefix("a=candidate:") {
            /* <foundation> <component> <transport> <priority> <address> <port> typ <type> ... */
            let fields: Vec<&str> = candidate.split(' ').collect();
            if let [_, _, transport, _, address, port, "typ", "host", ..] = fields[..] {
                if let (true, Ok(ip), Ok(port)) = (
                    transport.eq_ignore_ascii_case("udp"),
                    address.parse(),
                    port.parse(),
                ) {
                    candidates.push(SocketAddr::new(ip, port));
                }
            }
            continue;
        }
        if let Some(media) = line.strip_prefix("m=") {
            /* m=<media> <port> <proto> <fmt> */
            port = media
                .split(' ')
                .nth(1)
                .and_then(|port| port.parse::<u16>().ok());
        }
        sdp.push_str(line);
        sdp.push_str("\r\n");
    }
    let port = match port {
        Some(0 | DISCARD_PORT) | None => return Err(SignalingError::NoPort),
        Some(port) => port,
    };

    let same_ip = candidates
        .iter()
        .find(|candidate| candidate.ip() == server.ip());
    /* A server reached over loopback runs on this machine, so any address it lists is local. */
    let same_machine = server
        .ip()
        .is_loopback()
        .then(|| {
            candidates
                .iter()
                .find(|candidate| candidate.is_ipv4() == server.is_ipv4())
        })
        .flatten();
    let target = same_ip
        .or(same_machine)
        .copied()
        .unwrap_or(SocketAddr::new(server.ip(), port));
    Ok(Answer { sdp, target })
}

/// The key the bridge signs its offers' `a=identity` with.
pub struct SignalingIdentity {
    key_pair: EcdsaKeyPair,
    cpk: String,
    rng: SystemRandom,
}

impl SignalingIdentity {
    pub fn generate() -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P384_SHA384_FIXED_SIGNING, &rng)
            .expect("system randomness is available");
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P384_SHA384_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .expect("generated key is valid");
        let mut spki = P384_SPKI_PREFIX.to_vec();
        spki.extend_from_slice(key_pair.public_key().as_ref());
        SignalingIdentity {
            key_pair,
            cpk: STANDARD.encode(spki),
            rng,
        }
    }

    fn sign(&self, header: &str, payload: &str) -> String {
        let signature = self
            .key_pair
            .sign(&self.rng, format!("{header}.{payload}").as_bytes())
            .expect("ECDSA signing does not fail with a valid key");
        URL_SAFE_NO_PAD.encode(signature.as_ref())
    }

    /// The `a=identity` value for an SDP: an RFC 8827 envelope holding a self-signed token
    /// naming this key, and this key's signature over the SDP's `a=fingerprint` lines.
    pub fn assert(&self, sdp: &str, now_secs: u64) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES384","typ":"JWT"}"#);
        let claims = json!({
            "cpk": self.cpk,
            "iss": IDENTITY_DOMAIN,
            "iat": now_secs,
            "exp": now_secs + IDENTITY_LIFETIME_SECS,
        });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let token = format!("{header}.{claims}.{}", self.sign(&header, &claims));

        let jws_header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES384"}"#);
        let fingerprints = URL_SAFE_NO_PAD.encode(canonical_fingerprint_json(sdp));
        let detached = format!("{jws_header}..{}", self.sign(&jws_header, &fingerprints));

        let assertion = json!({ "token": token, "fingerprints": detached }).to_string();
        let envelope = json!({
            "idp": { "domain": IDENTITY_DOMAIN, "protocol": "default" },
            "assertion": assertion,
        });
        STANDARD.encode(envelope.to_string())
    }

    /// Adds `a=identity` to an offer, among the session-level attributes.
    pub fn sign_offer(&self, offer: &str, now_secs: u64) -> String {
        let identity = format!("a=identity:{}\r\n", self.assert(offer, now_secs));
        match offer.find("\r\nm=") {
            Some(at) => format!("{}{identity}{}", &offer[..at + 2], &offer[at + 2..]),
            None => format!("{offer}{identity}"),
        }
    }
}

/// `{"fingerprint":[{"algorithm":..,"digest":..}]}` with sorted keys and no spaces.
fn canonical_fingerprint_json(sdp: &str) -> String {
    let entries: Vec<Value> = sdp
        .lines()
        .filter_map(|line| line.strip_prefix("a=fingerprint:"))
        .filter_map(|value| value.trim().split_once(' '))
        .map(|(algorithm, digest)| json!({ "algorithm": algorithm, "digest": digest }))
        .collect();
    json!({ "fingerprint": entries }).to_string()
}

#[cfg(test)]
mod tests {
    use ring::signature::{ECDSA_P384_SHA384_FIXED, UnparsedPublicKey};

    use super::*;

    const OFFER: &str = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n\
        m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\n\
        a=fingerprint:sha-256 00:11:22\r\na=setup:actpass\r\n";

    #[test]
    fn identity_signs_the_offer_fingerprints() {
        let identity = SignalingIdentity::generate();
        let signed = identity.sign_offer(OFFER, 1_000);
        let (before, after) = signed.split_once("a=identity:").unwrap();
        assert!(before.ends_with("t=0 0\r\n"));
        let (encoded, rest) = after.split_once("\r\n").unwrap();
        assert!(rest.starts_with("m=application"));

        let envelope: Value = serde_json::from_slice(&STANDARD.decode(encoded).unwrap()).unwrap();
        let assertion: Value =
            serde_json::from_str(envelope["assertion"].as_str().unwrap()).unwrap();
        let mut parts = assertion["fingerprints"].as_str().unwrap().split('.');
        let (header, empty, signature) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        assert_eq!(empty, "");

        let payload = r#"{"fingerprint":[{"algorithm":"sha-256","digest":"00:11:22"}]}"#;
        let signing_input = format!("{header}.{}", URL_SAFE_NO_PAD.encode(payload));
        UnparsedPublicKey::new(
            &ECDSA_P384_SHA384_FIXED,
            identity.key_pair.public_key().as_ref(),
        )
        .verify(
            signing_input.as_bytes(),
            &URL_SAFE_NO_PAD.decode(signature).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn answer_target_prefers_reachable_candidates() {
        const ANSWER: &str = "v=0\r\na=identity:abc\r\nm=application 19234 UDP/DTLS/SCTP webrtc-datachannel\r\n\
            a=candidate:1 1 udp 2122260223 10.1.50.251 19234 typ host generation 0 network-id 1\r\na=setup:active\r\n";
        let loopback: SocketAddr = "127.0.0.1:19232".parse().unwrap();
        let answer = prepare_answer(ANSWER, loopback).unwrap();
        assert_eq!(answer.target, "10.1.50.251:19234".parse().unwrap());
        assert!(!answer.sdp.contains("a=identity") && !answer.sdp.contains("a=candidate"));
        assert!(answer.sdp.contains("a=setup:active\r\n"));

        /* Another machine: its signaling address, unless it lists that address itself. */
        let remote: SocketAddr = "192.168.1.5:19232".parse().unwrap();
        assert_eq!(
            prepare_answer(ANSWER, remote).unwrap().target,
            "192.168.1.5:19234".parse().unwrap()
        );
        let listed: SocketAddr = "10.1.50.251:19232".parse().unwrap();
        assert_eq!(
            prepare_answer(ANSWER, listed).unwrap().target,
            "10.1.50.251:19234".parse().unwrap()
        );

        assert!(matches!(
            prepare_answer(
                "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n",
                loopback
            ),
            Err(SignalingError::NoPort)
        ));
    }

    #[test]
    fn status_parses_and_tolerates_empty_body() {
        let status = ServerStatus::parse(br#"{"name":"Box","level":"World","players":2,"maxPlayers":10,"protocol":924,"version":"1.26.60","gameType":1}"#);
        assert_eq!(status.name, "Box");
        assert_eq!(status.max_players, 10);
        assert_eq!(status.protocol, Some(924));
        assert_eq!(status.game_type, 1);

        let empty = ServerStatus::parse(b"");
        assert_eq!(empty.protocol, None);
        assert!(empty.name.is_empty());
    }
}
