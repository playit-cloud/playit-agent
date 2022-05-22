use byteorder::{BigEndian, WriteBytesExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::abs_diff;
use crate::hmac::HmacSha256;

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct Authentication {
    pub(crate) details: RequestDetails,
    pub(crate) signature: Signature,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct RequestDetails {
    pub(crate) account_id: u64,
    pub(crate) request_timestamp: u64,
    pub(crate) session_id: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum Signature {
    /* provided by API */
    System(SystemSignature),
    /* authenticates */
    Session(SessionSignature),
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct SystemSignature {
    pub(crate) signature: [u8; 32],
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct SessionSignature {
    pub(crate) session_timestamp: u64,
    pub(crate) session_signature: [u8; 32],
    pub(crate) signature: [u8; 32],
}

const MAX_API_TIME_DIFF: u64 = 60_000 /* 1 minute */;
const MAX_USER_TIME_DIFF: u64 = 300_000 /* 5 minutes */;

impl Signature {
    pub fn validate(
        &self,
        details: &RequestDetails,
        now: u64,
        data: &mut Vec<u8>,
        secret: &HmacSha256,
    ) -> Result<Authorization, SignatureError> {
        let (max_diff, from_system) = match self {
            Signature::System(_) => (MAX_API_TIME_DIFF, true),
            Signature::Session(_) => (MAX_USER_TIME_DIFF, false),
        };

        if abs_diff(details.request_timestamp, now) > max_diff {
            return Err(SignatureError::SignatureExpired {
                now,
                timestamp: details.request_timestamp,
                from_system,
            });
        }

        match self {
            Signature::System(s) => {
                s.validate(details, data, secret)
                    .map(|account_id| Authorization::SystemLevel {
                        account_id,
                        sig_epoch: details.request_timestamp,
                    })
            }
            Signature::Session(s) => {
                s.validate(details, now, data, secret)
                    .map(|(account_id, session_id)| Authorization::SessionLevel {
                        account_id,
                        session_id,
                        sig_epoch: s.session_timestamp,
                    })
            }
        }
    }
}

impl SystemSignature {
    fn validate(
        &self,
        details: &RequestDetails,
        data: &mut Vec<u8>,
        key: &HmacSha256,
    ) -> Result<u64, SignatureError> {
        let og_data_len = data.len();
        data.write_u64::<BigEndian>(details.account_id).unwrap();
        data.write_u64::<BigEndian>(details.request_timestamp)
            .unwrap();
        let verify = key.verify(data, &self.signature);
        data.truncate(og_data_len);

        verify.map_err(|_| SignatureError::InvalidSignature)?;

        Ok(details.account_id)
    }
}

impl SessionSignature {
    fn validate(
        &self,
        details: &RequestDetails,
        now: u64,
        data: &mut Vec<u8>,
        key: &HmacSha256,
    ) -> Result<(u64, u64), SignatureError> {
        let session_id = match details.session_id {
            Some(v) => v,
            None => return Err(SignatureError::MissingSessionId),
        };

        if abs_diff(self.session_timestamp, now) > MAX_API_TIME_DIFF {
            return Err(SignatureError::SignatureExpired {
                now,
                timestamp: self.session_timestamp,
                from_system: false,
            });
        }

        /* session validation and shared secret gen can be cached */

        /* validate session token */
        {
            let mut buffer = Vec::with_capacity(std::mem::size_of::<u64>() * 3);
            buffer.write_u64::<BigEndian>(details.account_id).unwrap();
            buffer.write_u64::<BigEndian>(session_id).unwrap();
            buffer
                .write_u64::<BigEndian>(self.session_timestamp)
                .unwrap();

            key.verify(&buffer, &self.session_signature)
                .map_err(|_| SignatureError::InvalidSessionToken)?;
        }

        /* generate shared secret */
        let shared_secret = key.sign(&self.session_signature);

        /* validate signature */
        {
            let key = HmacSha256::create(shared_secret.as_ref());

            let og_data_len = data.len();
            data.write_u64::<BigEndian>(details.account_id).unwrap();
            data.write_u64::<BigEndian>(details.request_timestamp)
                .unwrap();

            let sig = key.verify(data, &self.signature);
            data.truncate(og_data_len);

            sig.map_err(|_| SignatureError::InvalidSignature)?;
        }

        Ok((details.account_id, session_id))
    }

    pub fn create_signature(
        account_id: u64,
        session_id: u64,
        session_timestamp: u64,
        key: &HmacSha256,
    ) -> [u8; 32] {
        let mut buffer = Vec::with_capacity(std::mem::size_of::<u64>() * 3);
        buffer.write_u64::<BigEndian>(account_id).unwrap();
        buffer.write_u64::<BigEndian>(session_id).unwrap();
        buffer.write_u64::<BigEndian>(session_timestamp).unwrap();

        key.sign_fixed(&buffer)
    }

    pub fn generate_session_secret(token: &[u8], key: &HmacSha256) -> [u8; 32] {
        key.sign_fixed(token)
    }
}

pub fn generate_signature(
    account_id: u64,
    timestamp: u64,
    data: &mut Vec<u8>,
    secret: &[u8],
) -> [u8; 32] {
    let key = HmacSha256::create(secret);

    let og_data_len = data.len();
    data.write_u64::<BigEndian>(account_id).unwrap();
    data.write_u64::<BigEndian>(timestamp).unwrap();

    let sig = key.sign_fixed(data);
    data.truncate(og_data_len);

    sig
}

#[derive(Debug)]
pub enum Authorization {
    SystemLevel {
        account_id: u64,
        sig_epoch: u64,
    },
    SessionLevel {
        account_id: u64,
        session_id: u64,
        sig_epoch: u64,
    },
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum SignatureError {
    MissingSessionId,
    SignatureExpired {
        now: u64,
        timestamp: u64,
        from_system: bool,
    },
    InvalidSessionToken,
    InvalidSignature,
}
