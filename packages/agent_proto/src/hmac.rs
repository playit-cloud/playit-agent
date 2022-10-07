use std::fmt::{Debug, Formatter};

use hmac::{Hmac, Mac};
use sha2::Sha256;

#[derive(Clone)]
pub struct HmacSha256(Hmac<Sha256>);

impl Debug for HmacSha256 {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "HmacSha256")
    }
}

impl From<Hmac<Sha256>> for HmacSha256 {
    fn from(key: Hmac<Sha256>) -> Self {
        HmacSha256(key)
    }
}

impl HmacSha256 {
    pub fn create(secret: &[u8]) -> Self {
        HmacSha256(Hmac::<Sha256>::new_from_slice(secret).unwrap())
    }

    pub fn verify(&self, data: &[u8], sig: &[u8]) -> Result<(), ()> {
        let mut mac = self.0.clone();
        mac.update(data);
        mac.verify_slice(sig).map_err(|_| ())
    }

    pub fn sign(&self, data: &[u8]) -> [u8; 32] {
        let mut mac = self.0.clone();
        mac.update(data);
        mac.finalize().into_bytes().into()
    }

    pub fn sign_fixed(&self, data: &[u8]) -> [u8; 32] {
        self.sign(data)
    }
}