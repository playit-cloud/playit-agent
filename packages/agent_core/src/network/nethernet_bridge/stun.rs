//! Rewrites the USERNAME of STUN Binding requests.
//!
//! NXS admission tokens are the host's ICE ufrag, so usernames run to hundreds of bytes.
//! str0m's ICE agent only handles usernames up to 128 bytes. The session gives str0m a short
//! stand-in ufrag and swaps it for the token on the wire, re-signing each request.

use ring::hmac;

const HEADER: usize = 20;
const MAGIC_COOKIE: u32 = 0x2112_a442;
const BINDING_REQUEST: u16 = 0x0001;
const ATTR_USERNAME: u16 = 0x0006;
const ATTR_MESSAGE_INTEGRITY: u16 = 0x0008;
const ATTR_FINGERPRINT: u16 = 0x8028;
const FINGERPRINT_XOR: u32 = 0x5354_554e;

/// If `packet` is a Binding request whose USERNAME is `from`, returns it with USERNAME `to`
/// and a MESSAGE-INTEGRITY keyed with `password`.
pub fn rewrite_username(packet: &[u8], from: &str, to: &str, password: &str) -> Option<Vec<u8>> {
    if packet.len() < HEADER
        || u16::from_be_bytes([packet[0], packet[1]]) != BINDING_REQUEST
        || u32::from_be_bytes(packet[4..8].try_into().unwrap()) != MAGIC_COOKIE
    {
        return None;
    }

    let mut attrs = Vec::with_capacity(packet.len() + to.len());
    let mut matched = false;
    let mut at = HEADER;
    while at + 4 <= packet.len() {
        let kind = u16::from_be_bytes([packet[at], packet[at + 1]]);
        let len = u16::from_be_bytes([packet[at + 2], packet[at + 3]]) as usize;
        let value = packet.get(at + 4..at + 4 + len)?;
        at += 4 + len.div_ceil(4) * 4;

        match kind {
            ATTR_USERNAME => {
                if value != from.as_bytes() {
                    return None;
                }
                matched = true;
                push_attr(&mut attrs, ATTR_USERNAME, to.as_bytes());
            }
            /* Both are recomputed below; anything after them is not covered by integrity. */
            ATTR_MESSAGE_INTEGRITY | ATTR_FINGERPRINT => break,
            _ => push_attr(&mut attrs, kind, value),
        }
    }
    if !matched {
        return None;
    }

    let mut out = Vec::with_capacity(HEADER + attrs.len() + 24 + 8);
    out.extend_from_slice(&packet[..HEADER]);
    out.extend_from_slice(&attrs);

    /* MESSAGE-INTEGRITY covers the message with a length that already counts it. */
    set_length(&mut out, attrs.len() + 24);
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, password.as_bytes());
    let mac = hmac::sign(&key, &out);
    push_attr(&mut out, ATTR_MESSAGE_INTEGRITY, mac.as_ref());

    set_length(&mut out, attrs.len() + 24 + 8);
    let fingerprint = crc32(&out) ^ FINGERPRINT_XOR;
    push_attr(&mut out, ATTR_FINGERPRINT, &fingerprint.to_be_bytes());
    Some(out)
}

fn push_attr(out: &mut Vec<u8>, kind: u16, value: &[u8]) {
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn set_length(message: &mut [u8], attrs_len: usize) {
    message[2..4].copy_from_slice(&(attrs_len as u16).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::nethernet_bridge::admission::tests::binding_request;

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn rewritten_request_verifies_with_new_username() {
        let original = binding_request("longtoken:client", "first password");
        let rewritten =
            rewrite_username(&original, "longtoken:client", "s:client", "second password").unwrap();

        /* Strip FINGERPRINT and check MESSAGE-INTEGRITY the way a receiver does. */
        let without_fingerprint = &rewritten[..rewritten.len() - 8];
        let integrity_at = without_fingerprint.len() - 24;
        let mut covered = without_fingerprint[..integrity_at].to_vec();
        set_length(&mut covered, integrity_at - HEADER + 24);
        let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, b"second password");
        assert!(hmac::verify(&key, &covered, &without_fingerprint[integrity_at + 4..]).is_ok());

        assert_eq!(&rewritten[HEADER + 4..HEADER + 4 + 8], b"s:client");
        assert!(rewrite_username(&original, "other", "s:client", "x").is_none());
    }
}
