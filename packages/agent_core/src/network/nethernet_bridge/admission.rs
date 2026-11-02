//! Validates the NXS stateless admission token a player's first STUN packet carries.
//!
//! The signaling service answers each player's offer on the host's behalf. It seals what the
//! host needs (the client's ICE password and DTLS fingerprint) into the ICE ufrag it hands the
//! client, using an admission key only it and this host know. The host recovers the session
//! from that ufrag without ever talking to the service about the join.

use std::collections::HashMap;
use std::net::SocketAddr;

use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use ring::{aead, hmac};

const HEADER_PREFIX: &str = "NXS1";
const HEADER_LEN: usize = 8;
const MAX_UFRAG_LEN: usize = 256;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const FIXED_PLAINTEXT: usize = 67;
const MIN_ENVELOPE: usize = NONCE_LEN + FIXED_PLAINTEXT + 22 + TAG_LEN;
const MAX_ENVELOPE: usize = 186;
/// Tokens are minted with a 60 second lifetime; anything claiming longer is not ours.
const MAX_TOKEN_TTL_MS: u64 = 120_000;
const MAX_MESSAGE_SIZE: u32 = 262_144;

const STUN_HEADER: usize = 20;
const STUN_MAGIC_COOKIE: u32 = 0x2112_a442;
const STUN_BINDING_REQUEST: u16 = 0x0001;
const ATTR_USERNAME: u16 = 0x0006;
const ATTR_MESSAGE_INTEGRITY: u16 = 0x0008;
const MESSAGE_INTEGRITY_LEN: usize = 20;

pub fn audience(incarnation: &str) -> String {
    format!("nxs-stateless-host-v1/{incarnation}")
}

/// An admission key epoch the provider issued to this host.
#[derive(Clone, Debug)]
pub struct AdmissionKey {
    pub key_id: String,
    /// Used as ASCII bytes; the provider never asks hosts to decode it.
    pub secret: String,
    /// Epoch milliseconds after which tokens under this key are refused.
    pub retire_after: Option<u64>,
}

/// What the token tells the host about the player's half of the WebRTC session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    /// The host's ICE ufrag: the token itself.
    pub local_ufrag: String,
    pub local_password: String,
    pub remote_ufrag: String,
    pub remote_password: String,
    /// SHA-256 of the client's DTLS certificate.
    pub client_fingerprint: [u8; 32],
    pub client_sctp_port: u16,
    pub client_max_message_size: u32,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    NotBindingRequest,
    MalformedStun,
    MalformedUsername,
    UnknownKey,
    MalformedToken,
    /// The token was not sealed for this host incarnation, or was altered.
    Unsealed,
    Expired,
    BadIntegrity,
}

/// Opens tokens under the host's installed admission keys.
pub struct AdmissionKeys {
    audience: String,
    keys: Vec<AdmissionKey>,
}

impl AdmissionKeys {
    pub fn new(incarnation: &str, keys: Vec<AdmissionKey>) -> Self {
        AdmissionKeys {
            audience: audience(incarnation),
            keys,
        }
    }

    /// Checks a player's first STUN Binding request, including its MESSAGE-INTEGRITY.
    pub fn admit(&self, packet: &[u8], now_ms: u64) -> Result<Admitted, AdmissionError> {
        let stun = StunRequest::parse(packet)?;
        let (local_ufrag, remote_ufrag) = stun
            .username
            .split_once(':')
            .ok_or(AdmissionError::MalformedUsername)?;
        if !is_ice_chars(remote_ufrag, 4, 256) {
            return Err(AdmissionError::MalformedUsername);
        }

        let admitted = self.open(local_ufrag, remote_ufrag, now_ms)?;
        if !stun.verify_integrity(admitted.local_password.as_bytes()) {
            return Err(AdmissionError::BadIntegrity);
        }
        Ok(admitted)
    }

    fn open(
        &self,
        local_ufrag: &str,
        remote_ufrag: &str,
        now_ms: u64,
    ) -> Result<Admitted, AdmissionError> {
        if MAX_UFRAG_LEN < local_ufrag.len()
            || local_ufrag.len() <= HEADER_LEN
            || !local_ufrag.starts_with(HEADER_PREFIX)
        {
            return Err(AdmissionError::MalformedToken);
        }
        let header = &local_ufrag[..HEADER_LEN];
        let key_id = &header[HEADER_PREFIX.len()..];
        if !key_id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return Err(AdmissionError::MalformedToken);
        }

        let key = self
            .keys
            .iter()
            .find(|key| key.key_id == key_id)
            .filter(|key| key.retire_after.is_none_or(|at| now_ms < at))
            .ok_or(AdmissionError::UnknownKey)?;

        let encoded = &local_ufrag[HEADER_LEN..];
        let mut envelope = STANDARD_NO_PAD
            .decode(encoded)
            .map_err(|_| AdmissionError::MalformedToken)?;
        /* Re-encoding must reproduce the ufrag, so each token has exactly one spelling. */
        if STANDARD_NO_PAD.encode(&envelope) != encoded
            || envelope.len() < MIN_ENVELOPE
            || MAX_ENVELOPE < envelope.len()
        {
            return Err(AdmissionError::MalformedToken);
        }

        let secret = key.secret.as_bytes();
        let aead_key = hmac_sha256(
            secret,
            &[b"nxs-stateless-aead-v1", b"\0", self.audience.as_bytes()],
        );
        let aad = [
            b"nxs-stateless-admission-v1".as_slice(),
            b"\0",
            header.as_bytes(),
            b"\0",
            self.audience.as_bytes(),
            b"\0",
            remote_ufrag.as_bytes(),
        ]
        .concat();

        let nonce: [u8; NONCE_LEN] = envelope[..NONCE_LEN].try_into().unwrap();
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &aead_key).expect("32 byte key"),
        );
        let plaintext = key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(&aad),
                &mut envelope[NONCE_LEN..],
            )
            .map_err(|_| AdmissionError::Unsealed)?;

        let claims = Claims::parse(plaintext).ok_or(AdmissionError::MalformedToken)?;
        let expires_at_ms = claims.expires_at_s as u64 * 1000;
        if expires_at_ms <= now_ms || MAX_TOKEN_TTL_MS < expires_at_ms - now_ms {
            return Err(AdmissionError::Expired);
        }

        Ok(Admitted {
            local_ufrag: local_ufrag.to_string(),
            local_password: host_ice_password(secret, &self.audience, local_ufrag),
            remote_ufrag: remote_ufrag.to_string(),
            remote_password: claims.client_password,
            client_fingerprint: claims.client_fingerprint,
            client_sctp_port: claims.client_sctp_port,
            client_max_message_size: claims.client_max_message_size,
            expires_at_ms,
        })
    }
}

/// The ICE password the signaling service gave the client for this host.
pub(crate) fn host_ice_password(secret: &[u8], audience: &str, local_ufrag: &str) -> String {
    let mac = hmac_sha256(
        secret,
        &[
            b"nxs-stateless-ice-v1",
            b"\0",
            audience.as_bytes(),
            b"\0",
            local_ufrag.as_bytes(),
        ],
    );
    STANDARD_NO_PAD.encode(&mac[..24])
}

fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut ctx = hmac::Context::with_key(&key);
    for part in parts {
        ctx.update(part);
    }
    ctx.sign().as_ref().try_into().unwrap()
}

fn is_ice_chars(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

struct Claims {
    expires_at_s: u32,
    client_fingerprint: [u8; 32],
    client_sctp_port: u16,
    client_max_message_size: u32,
    client_password: String,
}

impl Claims {
    fn parse(data: &[u8]) -> Option<Claims> {
        if data.len() < FIXED_PLAINTEXT + 22 {
            return None;
        }
        let expires_at_s = u32::from_be_bytes(data[0..4].try_into().unwrap());
        let client_fingerprint: [u8; 32] = data[4..36].try_into().unwrap();
        let client_sctp_port = u16::from_be_bytes(data[36..38].try_into().unwrap());
        let client_max_message_size = u32::from_be_bytes(data[38..42].try_into().unwrap());
        /* 42..58 binds the player's login identity key; 58..66 is the NetherNet network id. */
        let password_len = data[66] as usize;
        if data.len() != FIXED_PLAINTEXT + password_len
            || client_sctp_port == 0
            || client_max_message_size == 0
            || MAX_MESSAGE_SIZE < client_max_message_size
        {
            return None;
        }
        let client_password = std::str::from_utf8(&data[FIXED_PLAINTEXT..]).ok()?;
        if !is_ice_chars(client_password, 22, 91) {
            return None;
        }
        Some(Claims {
            expires_at_s,
            client_fingerprint,
            client_sctp_port,
            client_max_message_size,
            client_password: client_password.to_string(),
        })
    }
}

struct StunRequest<'a> {
    packet: &'a [u8],
    username: &'a str,
    /// Offset of the MESSAGE-INTEGRITY attribute header.
    integrity_at: usize,
}

impl<'a> StunRequest<'a> {
    fn parse(packet: &'a [u8]) -> Result<Self, AdmissionError> {
        if packet.len() < STUN_HEADER {
            return Err(AdmissionError::NotBindingRequest);
        }
        let kind = u16::from_be_bytes([packet[0], packet[1]]);
        let len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
        let cookie = u32::from_be_bytes(packet[4..8].try_into().unwrap());
        if kind != STUN_BINDING_REQUEST || cookie != STUN_MAGIC_COOKIE {
            return Err(AdmissionError::NotBindingRequest);
        }
        if packet.len() != STUN_HEADER + len {
            return Err(AdmissionError::MalformedStun);
        }

        let mut username = None;
        let mut integrity_at = None;
        let mut at = STUN_HEADER;
        while at + 4 <= packet.len() {
            let attr = u16::from_be_bytes([packet[at], packet[at + 1]]);
            let attr_len = u16::from_be_bytes([packet[at + 2], packet[at + 3]]) as usize;
            let value = packet
                .get(at + 4..at + 4 + attr_len)
                .ok_or(AdmissionError::MalformedStun)?;
            match attr {
                ATTR_USERNAME if integrity_at.is_none() => {
                    username = Some(
                        std::str::from_utf8(value)
                            .map_err(|_| AdmissionError::MalformedUsername)?,
                    );
                }
                ATTR_MESSAGE_INTEGRITY => {
                    if attr_len != MESSAGE_INTEGRITY_LEN {
                        return Err(AdmissionError::MalformedStun);
                    }
                    integrity_at = Some(at);
                    break;
                }
                _ => {}
            }
            at += 4 + attr_len.div_ceil(4) * 4;
        }

        Ok(StunRequest {
            packet,
            username: username.ok_or(AdmissionError::MalformedUsername)?,
            integrity_at: integrity_at.ok_or(AdmissionError::BadIntegrity)?,
        })
    }

    /// RFC 5389 short-term credential check: HMAC-SHA1 over the message up to the
    /// MESSAGE-INTEGRITY attribute, with the header length covering that attribute.
    fn verify_integrity(&self, password: &[u8]) -> bool {
        let covered_len = (self.integrity_at + 4 + MESSAGE_INTEGRITY_LEN - STUN_HEADER) as u16;
        let mut header = [0u8; STUN_HEADER];
        header.copy_from_slice(&self.packet[..STUN_HEADER]);
        header[2..4].copy_from_slice(&covered_len.to_be_bytes());

        let mut covered = header.to_vec();
        covered.extend_from_slice(&self.packet[STUN_HEADER..self.integrity_at]);

        let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, password);
        let start = self.integrity_at + 4;
        hmac::verify(
            &key,
            &covered,
            &self.packet[start..start + MESSAGE_INTEGRITY_LEN],
        )
        .is_ok()
    }
}

/// Each token admits one UDP source. A replay from another address is refused until the
/// token expires.
#[derive(Default)]
pub struct UsedTokens {
    used: HashMap<String, (SocketAddr, u64)>,
}

impl UsedTokens {
    const MAX_TRACKED: usize = 4096;

    pub fn claim(&mut self, admitted: &Admitted, source: SocketAddr, now_ms: u64) -> bool {
        self.used.retain(|_, (_, expires_at)| now_ms < *expires_at);
        match self.used.get(&admitted.local_ufrag) {
            Some((claimed_by, _)) => *claimed_by == source,
            None if Self::MAX_TRACKED <= self.used.len() => false,
            None => {
                self.used.insert(
                    admitted.local_ufrag.clone(),
                    (source, admitted.expires_at_ms),
                );
                true
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /* From dynamic-firewall's nethernet-signaling/tests/fixtures/stateless-admission-v1.fixtures.json. */
    pub(crate) const SECRET: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
    pub(crate) const INCARNATION: &str = "0123456789abcdef0123456789abcdef";
    const CLIENT_UFRAG: &str = "clientFixtureUf";
    const NOW_MS: u64 = 1788484800000;

    pub(crate) fn fixture_keys() -> AdmissionKeys {
        AdmissionKeys::new(
            INCARNATION,
            vec![AdmissionKey {
                key_id: "K001".into(),
                secret: SECRET.into(),
                retire_after: None,
            }],
        )
    }

    /// Mints a token the way the signaling service does, for tests that need a full STUN packet.
    pub(crate) fn mint(
        keys: &AdmissionKeys,
        client_ufrag: &str,
        client_password: &str,
        fingerprint: [u8; 32],
        expires_at_s: u32,
    ) -> String {
        let key = &keys.keys[0];
        let header = format!("NXS1{}", key.key_id);
        let mut plaintext = Vec::new();
        plaintext.extend_from_slice(&expires_at_s.to_be_bytes());
        plaintext.extend_from_slice(&fingerprint);
        plaintext.extend_from_slice(&5000u16.to_be_bytes());
        plaintext.extend_from_slice(&262144u32.to_be_bytes());
        plaintext.extend_from_slice(&[0; 16]);
        plaintext.extend_from_slice(&7u64.to_be_bytes());
        plaintext.push(client_password.len() as u8);
        plaintext.extend_from_slice(client_password.as_bytes());

        let aead_key = hmac_sha256(
            key.secret.as_bytes(),
            &[b"nxs-stateless-aead-v1", b"\0", keys.audience.as_bytes()],
        );
        let aad = [
            b"nxs-stateless-admission-v1".as_slice(),
            b"\0",
            header.as_bytes(),
            b"\0",
            keys.audience.as_bytes(),
            b"\0",
            client_ufrag.as_bytes(),
        ]
        .concat();
        let nonce = [7u8; 12];
        let sealing =
            aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, &aead_key).unwrap());
        sealing
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(&aad),
                &mut plaintext,
            )
            .unwrap();
        let mut envelope = nonce.to_vec();
        envelope.extend_from_slice(&plaintext);
        format!("{header}{}", STANDARD_NO_PAD.encode(envelope))
    }

    pub(crate) fn binding_request(username: &str, password: &str) -> Vec<u8> {
        let mut attrs = Vec::new();
        attrs.extend_from_slice(&ATTR_USERNAME.to_be_bytes());
        attrs.extend_from_slice(&(username.len() as u16).to_be_bytes());
        attrs.extend_from_slice(username.as_bytes());
        while attrs.len() % 4 != 0 {
            attrs.push(0);
        }
        let mut packet = Vec::new();
        packet.extend_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
        packet.extend_from_slice(&((attrs.len() + 24) as u16).to_be_bytes());
        packet.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        packet.extend_from_slice(&[1; 12]);
        packet.extend_from_slice(&attrs);

        let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, password.as_bytes());
        let mac = hmac::sign(&key, &packet);
        packet.extend_from_slice(&ATTR_MESSAGE_INTEGRITY.to_be_bytes());
        packet.extend_from_slice(&20u16.to_be_bytes());
        packet.extend_from_slice(mac.as_ref());
        packet
    }

    #[test]
    fn opens_fixture_token() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "test_fixtures/stateless-admission-v1.fixtures.json"
        ))
        .unwrap();
        let full_ufrag = fixture["expected"]["localUfrag"].as_str().unwrap();

        let admitted = fixture_keys()
            .open(full_ufrag, CLIENT_UFRAG, NOW_MS)
            .unwrap();
        assert_eq!(
            admitted.local_password,
            fixture["expected"]["icePwd"].as_str().unwrap()
        );
        assert_eq!(admitted.remote_password, "clientPassword01234567890");
        assert_eq!(
            hex::encode(admitted.client_fingerprint),
            fixture["claims"]["clientFingerprintHex"].as_str().unwrap()
        );
        assert_eq!(admitted.client_sctp_port, 5000);
        assert_eq!(admitted.client_max_message_size, 262144);
        assert_eq!(admitted.expires_at_ms, 1788484830000);

        assert_eq!(
            fixture_keys().open(full_ufrag, "otherClientUfrag", NOW_MS),
            Err(AdmissionError::Unsealed)
        );
        assert_eq!(
            fixture_keys().open(full_ufrag, CLIENT_UFRAG, 1788484830000),
            Err(AdmissionError::Expired)
        );
        let other_incarnation =
            AdmissionKeys::new("ffffffffffffffffffffffffffffffff", fixture_keys().keys);
        assert_eq!(
            other_incarnation.open(full_ufrag, CLIENT_UFRAG, NOW_MS),
            Err(AdmissionError::Unsealed)
        );
    }

    #[test]
    fn admits_signed_binding_request_once_per_source() {
        let keys = fixture_keys();
        let token = mint(
            &keys,
            "abcdUfrag",
            "clientPassword01234567890",
            [0xab; 32],
            (NOW_MS / 1000) as u32 + 60,
        );
        let password = host_ice_password(SECRET.as_bytes(), &keys.audience, &token);

        let packet = binding_request(&format!("{token}:abcdUfrag"), &password);
        let admitted = keys.admit(&packet, NOW_MS).unwrap();
        assert_eq!(admitted.remote_ufrag, "abcdUfrag");

        let forged = binding_request(
            &format!("{token}:abcdUfrag"),
            "wrong password wrong password!!",
        );
        assert_eq!(
            keys.admit(&forged, NOW_MS),
            Err(AdmissionError::BadIntegrity)
        );

        let mut used = UsedTokens::default();
        let a: SocketAddr = "127.0.0.1:1000".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        assert!(used.claim(&admitted, a, NOW_MS));
        assert!(used.claim(&admitted, a, NOW_MS));
        assert!(!used.claim(&admitted, b, NOW_MS));
    }
}
