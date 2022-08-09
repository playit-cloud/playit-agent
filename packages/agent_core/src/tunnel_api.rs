use std::sync::atomic::{AtomicI64, Ordering};
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use playit_agent_common::{AgentRegistered, ClaimLease, Ping, RpcMessage, TunnelRequest};
use playit_agent_common::api::SessionSecret;
use playit_agent_common::rpc::SignedRpcRequest;

use super::api_client::{ApiClient, ApiError};
use super::tunnel_io::TunnelIO;
use crate::now_milli;

pub struct TunnelApi {
    client: ApiClient,
    io: RwLock<TunnelIO>,
    session: RwLock<Option<Session>>,
    time_adjust: AtomicI64,
}

pub struct Session {
    registered: AgentRegistered,
    shared_secret: [u8; 32],
}

impl TunnelApi {
    pub fn new(api: ApiClient, io: TunnelIO) -> Self {
        TunnelApi {
            client: api,
            io: RwLock::new(io),
            session: RwLock::new(None),
            time_adjust: AtomicI64::new(0),
        }
    }

    pub fn client_api(&self) -> &ApiClient {
        &self.client
    }

    /* will be added to system clock when using timestamps with tunnel server */
    pub fn set_time_adjust(&self, adjust: i64) {
        self.time_adjust.store(adjust, Ordering::SeqCst);
    }

    pub async fn io(&self) -> RwLockReadGuard<TunnelIO> {
        self.io.read().await
    }

    pub async fn io_mut(&self) -> RwLockWriteGuard<TunnelIO> {
        self.io.write().await
    }

    pub fn ping(&self, id: u64) -> SignedRpcRequest<TunnelRequest> {
        SignedRpcRequest::new_unsigned(TunnelRequest::Ping(Ping {
            id,
        }))
    }

    pub async fn request_register(&self) -> Result<RpcMessage<SignedRpcRequest<TunnelRequest>>, TunnelApiError> {
        self.send_signed(TunnelRequest::RegisterAgent, None).await
    }

    pub async fn claim_lease(&self, claim: ClaimLease, request_id: Option<u64>) -> Result<RpcMessage<SignedRpcRequest<TunnelRequest>>, TunnelApiError> {
        self.send_signed(TunnelRequest::ClaimLeaseV2(claim), request_id).await
    }

    pub async fn keep_alive(&self) -> Result<RpcMessage<SignedRpcRequest<TunnelRequest>>, TunnelApiError> {
        self.send_session_signed(TunnelRequest::KeepAlive).await
    }

    pub async fn setup_udp_channel(&self) -> Result<RpcMessage<SignedRpcRequest<TunnelRequest>>, TunnelApiError> {
        self.send_session_signed(TunnelRequest::SetupUdpChannel).await
    }

    pub async fn register(&self, registered: AgentRegistered) -> Result<(), TunnelApiError> {
        let SessionSecret {
            agent_registered,
            secret,
        } = self.client.generate_shared_tunnel_secret(registered).await?;

        let shared_secret = match hex::decode(secret) {
            Ok(bytes) => {
                if bytes.len() != 32 {
                    tracing::error!(
                        length = bytes.len(),
                        "expected shared secret to be of length 32"
                    );
                    return Err(TunnelApiError::FailedToParseSharedSecret);
                }
                let mut data = [0u8; 32];
                data.copy_from_slice(&bytes);
                data
            }
            Err(error) => {
                tracing::error!(?error, "failed to parse shared secret provided by api");
                return Err(TunnelApiError::FailedToParseSharedSecret);
            }
        };

        let mut lock = self.session.write().await;
        lock.replace(Session {
            registered: agent_registered,
            shared_secret,
        });

        Ok(())
    }

    async fn send_session_signed(&self, data: TunnelRequest) -> Result<RpcMessage<SignedRpcRequest<TunnelRequest>>, TunnelApiError> {
        let session_opt = self.session.read().await;
        let session = match session_opt.as_ref() {
            Some(v) => v,
            None => return Err(TunnelApiError::SessionNotRegistered),
        };

        let now = self.clock();
        let req = SignedRpcRequest::new_session_signed(
            &session.registered,
            &session.shared_secret,
            now,
            data,
        );

        Ok(self.io().await.send(req).await?)
    }

    async fn send_unsigned(&self, req: TunnelRequest) -> Result<RpcMessage<SignedRpcRequest<TunnelRequest>>, TunnelApiError> {
        let req = SignedRpcRequest::new_unsigned(req);
        Ok(self.io().await.send(req).await?)
    }

    async fn send_signed(&self, req: TunnelRequest, request_id: Option<u64>) -> Result<RpcMessage<SignedRpcRequest<TunnelRequest>>, TunnelApiError> {
        let req = self.client.sign_tunnel_request(req).await?;
        tracing::info!(?req, "request signed");

        let io = self.io().await;
        match request_id {
            Some(request_id) => {
                let req = RpcMessage {
                    request_id,
                    content: req,
                };

                io.send_raw(&req).await?;
                Ok(req)
            }
            None => Ok(io.send(req).await?)
        }
    }

    fn clock(&self) -> u64 {
        (now_milli() as i64 + self.time_adjust.load(Ordering::SeqCst)) as u64
    }
}

#[derive(Debug)]
pub enum TunnelApiError {
    ApiError(ApiError),
    IoError(std::io::Error),
    SessionNotRegistered,
    FailedToParseSharedSecret,
}

impl From<ApiError> for TunnelApiError {
    fn from(err: ApiError) -> Self {
        TunnelApiError::ApiError(err)
    }
}

impl From<std::io::Error> for TunnelApiError {
    fn from(err: std::io::Error) -> Self {
        TunnelApiError::IoError(err)
    }
}