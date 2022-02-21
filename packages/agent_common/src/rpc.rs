use std::borrow::Cow;
use std::fmt::{Debug, Formatter};
use std::marker::PhantomData;

use byteorder::{BigEndian, WriteBytesExt};
use ring::hmac;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::auth::{
    Authentication, Authorization, RequestDetails, SessionSignature, Signature, SignatureError,
    SystemSignature,
};
use crate::AgentRegistered;

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SignedRpcRequest<T: DeserializeOwned + Serialize> {
    auth: Option<Authentication>,
    content: Vec<u8>,
    #[serde(skip_serializing, default)]
    _phantom: PhantomData<T>,
}

impl<T: DeserializeOwned + Serialize + Debug> Debug for SignedRpcRequest<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SignedRpcRequest {{ content: {:?}, auth: {} }} ",
            self.content,
            match &self.auth {
                None => Cow::Borrowed("None"),
                Some(auth) => {
                    match &auth.signature {
                        Signature::System(_) => {
                            Cow::Owned(format!("System {{ details: {:?} }}", auth.details))
                        }
                        Signature::Session(s) => Cow::Owned(format!(
                            "Session {{ details: {:?}, ts: {} }}",
                            auth.details, s.session_timestamp
                        )),
                    }
                }
            }
        )
    }
}

impl<T: DeserializeOwned + Serialize> SignedRpcRequest<T> {
    pub fn new_unsigned<K: Into<T>>(data: K) -> Self {
        let data = bincode::serialize(&data.into()).unwrap();

        SignedRpcRequest {
            auth: None,
            content: data,
            _phantom: Default::default(),
        }
    }

    pub fn new_session_signed<K: Into<T>>(
        session: &AgentRegistered,
        shared_secret: &[u8],
        timestamp: u64,
        data: K,
    ) -> Self {
        let mut data = bincode::serialize(&data.into()).unwrap();

        let signature = {
            let og_data_len = data.len();
            data.write_u64::<BigEndian>(session.account_id).unwrap();
            data.write_u64::<BigEndian>(timestamp).unwrap();

            let key = hmac::Key::new(hmac::HMAC_SHA256, shared_secret);
            let sig = hmac::sign(&key, &data);

            data.truncate(og_data_len);

            let mut data = [0u8; 32];
            data.copy_from_slice(sig.as_ref());
            data
        };

        SignedRpcRequest {
            auth: Some(Authentication {
                details: RequestDetails {
                    account_id: session.account_id,
                    request_timestamp: timestamp,
                    session_id: Some(session.session_id),
                },
                signature: Signature::Session(SessionSignature {
                    session_timestamp: session.session_timestamp,
                    session_signature: session.signature,
                    signature,
                }),
            }),
            content: data,
            _phantom: Default::default(),
        }
    }

    pub fn new_system_signed<K: Into<T>>(
        key: &hmac::Key,
        account_id: u64,
        timestamp: u64,
        data: K,
    ) -> Self {
        let mut data = bincode::serialize(&data.into()).unwrap();

        let signature = {
            let og_data_len = data.len();
            data.write_u64::<BigEndian>(account_id).unwrap();
            data.write_u64::<BigEndian>(timestamp).unwrap();

            let sig = hmac::sign(key, &data);

            data.truncate(og_data_len);

            let mut data = [0u8; 32];
            data.copy_from_slice(sig.as_ref());
            data
        };

        SignedRpcRequest {
            auth: Some(Authentication {
                details: RequestDetails {
                    account_id,
                    request_timestamp: timestamp,
                    session_id: None,
                },
                signature: Signature::System(SystemSignature { signature }),
            }),
            content: data,
            _phantom: Default::default(),
        }
    }

    pub fn authenticate(
        &mut self,
        now: u64,
        secret: &hmac::Key,
    ) -> Result<Option<Authorization>, SignatureError> {
        let auth = match &self.auth {
            Some(v) => v,
            None => return Ok(None),
        };

        auth.signature
            .validate(&auth.details, now, &mut self.content, secret)
            .map(Some)
    }

    pub fn into_content(self) -> Result<T, Self> {
        match bincode::deserialize(&self.content) {
            Ok(v) => Ok(v),
            Err(_) => Err(self),
        }
    }

    pub fn content_slice(&self) -> &[u8] {
        &self.content
    }
}

#[cfg(test)]
mod test {
    use crate::rpc::SignedRpcRequest;

    #[test]
    fn test() {
        let v = SignedRpcRequest::<String>::new_unsigned("hello world".to_string());
        let json = serde_json::to_string(&v).unwrap();
        let _parse: SignedRpcRequest<String> = serde_json::from_str(&json).unwrap();
        println!("{}", json);
    }
}
