use std::error::Error;
use std::fmt::{Display, Formatter};

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;


use playit_agent_proto::control_feed::ControlFeed;
use playit_agent_proto::control_messages::{ControlRequest, ControlResponse, Ping, Pong};
use playit_agent_proto::encoding::MessageEncoding;
use playit_agent_proto::raw_slice::RawSlice;
use playit_agent_proto::rpc::ControlRpcMessage;
use crate::api::api::{AgentVersion, ApiError, ApiErrorNoFail, ApiResponseError, Platform, PlayitAgentVersion, ReqProtoRegister};
use crate::api::http_client::HttpClientError;
use crate::api::PlayitApi;

use crate::utils::now_milli;
use crate::tunnel::control::AuthenticatedControl;
use crate::utils::error_helper::ErrorHelper;

pub struct SetupFindSuitableChannel {
    options: Vec<SocketAddr>,
}

impl SetupFindSuitableChannel {
    pub fn new(options: Vec<SocketAddr>) -> Self {
        SetupFindSuitableChannel { options }
    }

    pub async fn setup(self) -> Result<ConnectedControl, SetupError> {
        let mut buffer: Vec<u8> = Vec::new();

        for addr in self.options {
            tracing::info!(?addr, "trying to establish tunnel connection");

            let socket = match UdpSocket::bind(match addr {
                SocketAddr::V4(_) => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
                SocketAddr::V6(_) => SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)),
            }).await {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, is_ip6 = addr.is_ipv6(), "failed to bind to UdpSocket");
                    continue;
                }
            };

            for _ in 0..3 {
                buffer.clear();

                ControlRpcMessage {
                    request_id: 1,
                    content: ControlRequest::Ping(Ping {
                        now: now_milli(),
                        current_ping: None,
                        session_id: None
                    }),
                }.write_to(&mut buffer)?;

                if let Err(error) = socket.send_to(&buffer, addr).await {
                    tracing::error!(?error, ?addr, "failed to send initial ping");
                    break;
                }

                buffer.resize(2048, 0);

                for _ in 0..3 {
                    let res = tokio::time::timeout(
                        Duration::from_millis(500),
                        socket.recv_from(&mut buffer),
                    ).await;

                    match res {
                        Ok(Ok((bytes, peer))) => {
                            if peer != addr {
                                tracing::error!(?peer, ?addr, "got message from different source");
                                continue;
                            }

                            let mut reader = &buffer[..bytes];
                            match ControlFeed::read_from(&mut reader) {
                                Ok(ControlFeed::Response(msg)) => {
                                    if msg.request_id != 1 {
                                        tracing::error!(?msg, "got response with unexpected request_id");
                                        continue;
                                    }

                                    match msg.content {
                                        ControlResponse::Pong(pong) => return Ok(ConnectedControl {
                                            control_addr: addr,
                                            udp: Arc::new(socket),
                                            pong,
                                        }),
                                        other => {
                                            tracing::error!(?other, "expected pong got other response");
                                        }
                                    }
                                }
                                Ok(other) => {
                                    tracing::error!(?other, "unexpected control feed");
                                }
                                Err(error) => {
                                    tracing::error!(?error, test = ?(), "failed to parse response data");
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            tracing::error!(?error, "failed to receive UDP packet");
                        }
                        Err(_) => {
                            tracing::error!("timeout waiting for Pong");
                            break;
                        }
                    }
                }
            }
        }

        Err(SetupError::FailedToConnect)
    }
}

fn get_platform() -> Platform {
    #[cfg(target_os = "window")]
    return Platform::Windows;

    #[cfg(target_os = "linux")]
    return Platform::Linux;

    #[cfg(target_os = "macos")]
    return Platform::Macos;

    #[cfg(target_os = "android")]
    return Platform::Android;

    #[cfg(target_os = "ios")]
    return Platform::Ios;

    #[allow(unreachable_code)]
    Platform::Unknown
}

#[derive(Debug)]
pub struct ConnectedControl {
    pub(crate) control_addr: SocketAddr,
    pub(crate) udp: Arc<UdpSocket>,
    pub(crate) pong: Pong,
}

impl ConnectedControl {
    pub async fn authenticate(self, secret_key: String) -> Result<AuthenticatedControl, SetupError> {
        let api = PlayitApi::create("https://api.playit.gg".to_string(), Some(secret_key.clone()));

        let res = api.proto_register(ReqProtoRegister {
            agent_version: PlayitAgentVersion {
                version: AgentVersion {
                    platform: get_platform(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                official: true,
                details_website: None,
            },
            client_addr: self.pong.client_addr,
            tunnel_addr: self.pong.tunnel_addr,
        }).await.with_error(|error| tracing::error!(?error, "failed to sign and register"))?;

        let bytes = match hex::decode(&res.key) {
            Ok(data) => data,
            Err(_) => return Err(SetupError::FailedToDecodeSignedAgentRegisterHex),
        };

        let mut buffer = Vec::new();

        for _ in 0..5 {
            buffer.clear();

            ControlRpcMessage {
                request_id: 10,
                content: RawSlice(&bytes)
            }.write_to(&mut buffer)?;

            self.udp.send_to(&buffer, self.control_addr).await?;

            for _ in 0..5 {
                buffer.resize(1024, 0);
                match tokio::time::timeout(Duration::from_millis(500), self.udp.recv_from(&mut buffer)).await {
                    Ok(Ok((bytes, remote))) => {
                        if remote != self.control_addr {
                            tracing::warn!("got response not from tunnel server");
                            continue;
                        }

                        let mut reader = &buffer[..bytes];
                        match ControlFeed::read_from(&mut reader) {
                            Ok(ControlFeed::Response(response)) => {
                                if response.request_id != 10 {
                                    tracing::error!(?response, "got response for different request");
                                    continue;
                                }

                                return match response.content {
                                    ControlResponse::RequestQueued => {
                                        tracing::info!("register queued, waiting 1s");
                                        tokio::time::sleep(Duration::from_secs(1)).await;
                                        break;
                                    }
                                    ControlResponse::AgentRegistered(registered) => {
                                        let pong = self.pong.clone();

                                        Ok(AuthenticatedControl {
                                            secret_key,
                                            api_client: api,
                                            conn: self,
                                            last_pong: pong,
                                            registered,
                                            buffer,
                                            current_ping: None,
                                        })
                                    },
                                    ControlResponse::InvalidSignature => Err(SetupError::RegisterInvalidSignature),
                                    ControlResponse::Unauthorized => Err(SetupError::RegisterUnauthorized),
                                    other => {
                                        tracing::error!(?other, "expected AgentRegistered but got something else");
                                        continue;
                                    }
                                }
                            }
                            Ok(other) => {
                                tracing::error!(?other, "got unexpected response from register request");
                                continue;
                            }
                            Err(error) => {
                                tracing::error!(?error, "failed to read response from tunnel");
                                continue;
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::error!(?error, "got error reading from socket");
                        break;
                    }
                    Err(_) => {
                        tracing::error!("timeout waiting for register response");
                        break;
                    }
                }
            }
        }

        Err(SetupError::FailedToConnect)
    }
}

#[derive(Debug)]
pub enum SetupError {
    IoError(std::io::Error),
    FailedToConnect,
    ApiFail(String),
    ApiError(ApiResponseError),
    RequestError(HttpClientError),
    FailedToDecodeSignedAgentRegisterHex,
    NoResponseFromAuthenticate,
    RegisterInvalidSignature,
    RegisterUnauthorized,
}

impl<F: serde::Serialize> From<ApiError<F, HttpClientError>> for SetupError {
    fn from(value: ApiError<F, HttpClientError>) -> Self {
        match value {
            ApiError::ApiError(api) => SetupError::ApiError(api),
            ApiError::ClientError(error) => SetupError::RequestError(error),
            ApiError::Fail(fail) => SetupError::ApiFail(serde_json::to_string(&fail).unwrap())
        }
    }
}

impl From<ApiErrorNoFail<HttpClientError>> for SetupError {
    fn from(value: ApiErrorNoFail<HttpClientError>) -> Self {
        match value {
            ApiErrorNoFail::ApiError(api) => SetupError::ApiError(api),
            ApiErrorNoFail::ClientError(error) => SetupError::RequestError(error),
        }
    }
}

impl Display for SetupError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for SetupError {
}

impl From<std::io::Error> for SetupError {
    fn from(e: std::io::Error) -> Self {
        SetupError::IoError(e)
    }
}
