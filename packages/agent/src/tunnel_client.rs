use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Serialize;
use slab::Slab;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, RwLock};
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot::{
    channel as oneshot, Receiver as OneshotReceiver, Sender as OneshotSender,
};

use agent_common::{
    AgentRegistered, ClaimError, ClaimLease, NewClient, Ping, Pong, RpcMessage,
    SetupUdpChannelDetails, TunnelFeed, TunnelRequest, TunnelResponse,
};
use agent_common::api::SessionSecret;
use agent_common::auth::SignatureError;
use agent_common::rpc::SignedRpcRequest;

use crate::api_client::{ApiClient, ApiError};
use crate::dependent_task::DependentTask;

#[derive(Clone)]
#[allow(dead_code)]
pub struct TunnelClient {
    shared: Arc<Inner>,
    receive_task: DependentTask<()>,
}

pub struct Session {
    registered: AgentRegistered,
    shared_secret: [u8; 32],
}

struct Inner {
    api: ApiClient,
    udp: UdpSocket,
    control_addr: SocketAddr,
    requests: Mutex<Slab<QueuedRequest>>,
    new_client_tx: Sender<NewClient>,
    session: RwLock<Option<Session>>,
}

pub const RESEND_CHECK_INTERVAL: u64 = 1000;
pub const RESEND_TIMEOUT: u64 = 2000;

struct QueuedRequest {
    resend_at: u64,
    attempt: u64,
    request: RpcMessage<SignedRpcRequest<TunnelRequest>>,
    handler: OneshotSender<TunnelResponse>,
}

impl TunnelClient {
    pub async fn new(
        api: ApiClient,
        new_client_tx: Sender<NewClient>,
    ) -> Result<Self, TunnelClientError> {
        let udp = UdpSocket::bind(SocketAddr::new(IpAddr::V4(0.into()), 0)).await?;
        let control_addr = api.get_control_addr().await?;

        let inner = Inner {
            udp,
            api,
            control_addr,
            requests: Default::default(),
            new_client_tx,
            session: RwLock::new(None),
        };

        let shared = Arc::new(inner);

        Ok(TunnelClient {
            shared: shared.clone(),
            receive_task: DependentTask::new(tokio::spawn(ControlClientTask::new(shared).run())),
        })
    }

    pub async fn register(&self) -> Result<AgentRegistered, TunnelClientError> {
        self.shared.register().await
    }

    pub async fn ping(&self) -> Result<Pong, TunnelClientError> {
        let now = now_milli();

        let handle = self
            .shared
            .send_unsigned(TunnelRequest::Ping(Ping { id: now }))
            .await?;

        match handle.await {
            Ok(TunnelResponse::Pong(mut pong)) => {
                pong.id = now_milli() - pong.id;
                Ok(pong)
            }
            Ok(response) => {
                tracing::error!(?response, "Got invalid response for register");
                Err(TunnelClientError::InvalidResponse)
            }
            _ => Err(TunnelClientError::StoppedProcessing),
        }
    }

    pub async fn claim_lease(&self, claim: ClaimLease) -> Result<ClaimLease, TunnelClientError> {
        let handle = self
            .shared
            .send_system_signed(TunnelRequest::ClaimLease(claim))
            .await?;

        match handle.await {
            Ok(TunnelResponse::ClaimResponse(r)) => r.map_err(TunnelClientError::ClaimError),
            Ok(TunnelResponse::SignatureError(e)) => Err(TunnelClientError::SignatureError(e)),
            Ok(response) => {
                tracing::error!(?response, "Got invalid response for register");
                Err(TunnelClientError::InvalidResponse)
            }
            _ => Err(TunnelClientError::StoppedProcessing),
        }
    }

    pub async fn keep_alive(&self) -> Result<bool, TunnelClientError> {
        let handle = self
            .shared
            .send_session_signed(TunnelRequest::KeepAlive)
            .await?;

        match handle.await {
            Ok(TunnelResponse::KeptAlive(kept_alive)) => {
                tracing::info!(alive = kept_alive.alive, tunnel_server = %kept_alive.tunnel_server_id, "kept alive");
                Ok(kept_alive.alive)
            }
            Ok(TunnelResponse::SignatureError(e)) => Err(TunnelClientError::SignatureError(e)),
            Ok(response) => {
                tracing::error!(?response, "Got invalid response for register");
                Err(TunnelClientError::InvalidResponse)
            }
            _ => Err(TunnelClientError::StoppedProcessing),
        }
    }

    pub async fn setup_udp_channel(&self) -> Result<SetupUdpChannelDetails, TunnelClientError> {
        let handle = self
            .shared
            .send_session_signed(TunnelRequest::SetupUdpChannel)
            .await?;

        match handle.await {
            Ok(TunnelResponse::SetupUdpChannelDetails(details)) => {
                tracing::info!(?details, "setup udp channel");
                Ok(details)
            }
            Ok(TunnelResponse::SignatureError(e)) => Err(TunnelClientError::SignatureError(e)),
            Ok(response) => {
                tracing::error!(?response, "Got invalid response for register");
                Err(TunnelClientError::InvalidResponse)
            }
            _ => Err(TunnelClientError::StoppedProcessing),
        }
    }
}

impl Inner {
    async fn send_signed_request(
        &self,
        now: u64,
        request: SignedRpcRequest<TunnelRequest>,
    ) -> OneshotReceiver<TunnelResponse> {
        let (tx, rx) = oneshot();

        let payload = {
            let mut lock = self.requests.lock().await;
            let entry = lock.vacant_entry();

            let request_id = entry.key();

            let req = QueuedRequest {
                resend_at: now + RESEND_TIMEOUT,
                attempt: 0,
                request: RpcMessage {
                    request_id: request_id as u64,
                    content: request,
                },
                handler: tx,
            };

            let handle = entry.insert(req);
            handle.request.as_payload()
        };

        if let Err(error) = self.udp.send_to(&payload, self.control_addr).await {
            tracing::error!(?error, "failed to send UDP packet to control");
        }

        rx
    }

    async fn register(&self) -> Result<AgentRegistered, TunnelClientError> {
        let handle = self
            .send_system_signed(TunnelRequest::RegisterAgent)
            .await?;

        match handle.await {
            Ok(TunnelResponse::AgentRegistered(registered)) => {
                let SessionSecret {
                    agent_registered,
                    secret,
                } = self.api.generate_shared_tunnel_secret(registered).await?;

                let shared_secret = match hex::decode(secret) {
                    Ok(bytes) => {
                        if bytes.len() != 32 {
                            tracing::error!(
                                length = bytes.len(),
                                "expected shared secret to be of length 32"
                            );
                            return Err(TunnelClientError::InvalidResponse);
                        }
                        let mut data = [0u8; 32];
                        data.copy_from_slice(&bytes);
                        data
                    }
                    Err(error) => {
                        tracing::error!(?error, "failed to parse shared secret provided by api");
                        return Err(TunnelClientError::InvalidResponse);
                    }
                };

                let mut lock = self.session.write().await;

                lock.replace(Session {
                    registered: agent_registered.clone(),
                    shared_secret,
                });

                Ok(agent_registered)
            }
            Ok(TunnelResponse::SignatureError(e)) => Err(TunnelClientError::SignatureError(e)),
            Ok(response) => {
                tracing::error!(?response, "Got invalid response for register");
                Err(TunnelClientError::InvalidResponse)
            }
            _ => Err(TunnelClientError::StoppedProcessing),
        }
    }

    async fn send_session_signed<T: DeserializeOwned + Serialize + Into<TunnelRequest>>(
        &self,
        data: T,
    ) -> Result<OneshotReceiver<TunnelResponse>, TunnelClientError> {
        let session_opt = self.session.read().await;
        let session = session_opt.as_ref().ok_or(TunnelClientError::NoSession)?;
        let now = now_milli();
        let req = SignedRpcRequest::new_session_signed(
            &session.registered,
            &session.shared_secret,
            now,
            data.into(),
        );
        Ok(self.send_signed_request(now, req).await)
    }

    async fn send_system_signed<T: Into<TunnelRequest>>(
        &self,
        request: T,
    ) -> Result<OneshotReceiver<TunnelResponse>, TunnelClientError> {
        let signed = self.api.sign_tunnel_request(request.into()).await?;
        Ok(self.send_signed_request(now_milli(), signed).await)
    }

    async fn send_unsigned<T: Into<TunnelRequest>>(
        &self,
        request: T,
    ) -> Result<OneshotReceiver<TunnelResponse>, TunnelClientError> {
        Ok(self
            .send_signed_request(now_milli(), SignedRpcRequest::new_unsigned(request.into()))
            .await)
    }
}

#[derive(Debug)]
pub enum TunnelClientError {
    ApiError(ApiError),
    IoError(std::io::Error),

    InvalidResponse,
    StoppedProcessing,
    NoSession,
    ClaimError(ClaimError),
    SignatureError(SignatureError),
}

impl From<ApiError> for TunnelClientError {
    fn from(e: ApiError) -> Self {
        TunnelClientError::ApiError(e)
    }
}

impl From<std::io::Error> for TunnelClientError {
    fn from(e: std::io::Error) -> Self {
        TunnelClientError::IoError(e)
    }
}

struct ControlClientTask {
    shared: Arc<Inner>,
    recv_buffer: Vec<u8>,
}

impl ControlClientTask {
    fn new(shared: Arc<Inner>) -> Self {
        ControlClientTask {
            shared,
            recv_buffer: vec![0u8; 2048],
        }
    }

    async fn run(mut self) {
        let mut next_resend = now_milli() + RESEND_CHECK_INTERVAL;

        loop {
            let res = tokio::time::timeout(
                Duration::from_millis(RESEND_CHECK_INTERVAL + 1),
                self.receive_message(),
            )
                .await;

            match res {
                Ok(Some(msg)) => {
                    self.process_control_feed(msg).await;
                }
                Ok(None) => {
                    tracing::warn!("failed to parse control feed");
                }
                Err(_) => {}
            }

            let now = now_milli();
            if next_resend < now {
                next_resend = now + RESEND_CHECK_INTERVAL;
                self.resend_requests(now).await;
            }
        }
    }

    async fn resend_requests(&mut self, now: u64) {
        let mut locked = self.shared.requests.lock().await;
        let mut to_remove = Vec::new();

        for (request_id, request) in locked.iter_mut() {
            if request.resend_at < now {
                tracing::info!(request_id, "resend request");

                if let Err(error) = self
                    .shared
                    .udp
                    .send_to(&request.request.as_payload(), self.shared.control_addr)
                    .await
                {
                    tracing::error!(?error, "failed to send udp packet");
                    continue;
                }

                if request.attempt >= 3 {
                    to_remove.push(request_id);
                }

                request.attempt += 1;
                request.resend_at = now + (RESEND_TIMEOUT * (request.attempt + 1));
            }
        }

        for request_id in to_remove {
            let request = locked.remove(request_id);
            request.handler.send(TunnelResponse::Failed);
        }
    }

    async fn receive_message(&mut self) -> Option<TunnelFeed> {
        let (len, addr) = match self.shared.udp.recv_from(&mut self.recv_buffer).await {
            Ok(v) => v,
            Err(error) => {
                tracing::error!(?error, "failed to receive data from UDP socket");
                tokio::time::sleep(Duration::from_secs(1)).await;
                return None;
            }
        };

        if addr != self.shared.control_addr {
            tracing::warn!("got packet not from control addr");
            return None;
        }

        TunnelFeed::from_slice(&self.recv_buffer[..len])
    }

    async fn process_control_feed(&self, msg: TunnelFeed) {
        match msg {
            TunnelFeed::Response(response) => {
                let mut requests = self.shared.requests.lock().await;
                if let Some(req) = requests.try_remove(response.request_id as usize) {
                    if req.handler.send(response.content).is_err() {
                        tracing::error!("failed to send control response");
                    }
                }
            }
            TunnelFeed::NewClient(new_client) => {
                if let Err(error) = self.shared.new_client_tx.send(new_client).await {
                    tracing::error!(?error, "failed to inform agent of new client");
                }
            }
        };
    }
}

fn now_milli() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
