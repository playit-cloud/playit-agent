use std::fmt::{Debug, Formatter};
use ring::hmac::Key;

#[derive(Clone)]
pub struct HmacSha256(ring::hmac::Key);

impl Debug for HmacSha256 {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "HmacSha256")
    }
}

impl From<ring::hmac::Key> for HmacSha256 {
    fn from(key: ring::hmac::Key) -> Self {
        HmacSha256(key)
    }
}

impl HmacSha256 {
    pub fn create(secret: &[u8]) -> Self {
        HmacSha256(ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret))
    }

    pub fn verify(&self, data: &[u8], sig: &[u8]) -> Result<(), ring::error::Unspecified> {
        ring::hmac::verify(&self.0, data, sig)
    }

    pub fn sign(&self, data: &[u8]) -> ring::hmac::Tag {
        ring::hmac::sign(&self.0, data)
    }

    pub fn sign_fixed(&self, data: &[u8]) -> [u8; 32] {
        let sig = self.sign(data);
        let mut out = [0u8; 32];
        out.copy_from_slice(sig.as_ref());
        out
    }
}