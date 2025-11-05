use std::marker::PhantomData;
use std::sync::Arc;

use futures_util::StreamExt;
use playit_agent_core::utils::id_slab::IdSlab;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tipsy::{Endpoint, ServerId};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

pub trait ServiceProto: 'static {
    type Request: Serialize + DeserializeOwned + Send + 'static;
    type Response: Serialize + DeserializeOwned + Send + 'static;

    fn socket_id() -> &'static str;
}

pub struct ServiceServer<S: ServiceProto>(PhantomData<S>);
pub struct ServiceClient<S: ServiceProto> {
    handlers: Arc<Mutex<Option<IdSlab<oneshot::Sender<WrappedMsg<S::Response>>>>>>,
    to_send: Sender<WireMessage<S::Request>>,
    cancel: CancellationToken,
    _phantom: PhantomData<S>,
}

impl<S: ServiceProto> ServiceClient<S> {
    pub async fn connect() -> Result<Self, std::io::Error> {
        let connect = Endpoint::connect(ServerId::new(S::socket_id())).await?;
        let (read, mut write) = tokio::io::split(connect);
        let cancel = CancellationToken::new();
        let handlers = Arc::new(Mutex::new(Some(IdSlab::<
            oneshot::Sender<WrappedMsg<S::Response>>,
        >::with_capacity(1024))));

        let read_cancel = cancel.clone();
        let read_handlers = handlers.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            loop {
                let json_line = match lines.next_line().await {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        tracing::info!("server closed connection");
                        break;
                    }
                    Err(error) => {
                        tracing::error!(?error, "got error reading next line from server");
                        break;
                    }
                };

                let response = match serde_json::from_str::<WireMessage<S::Response>>(&json_line) {
                    Ok(res) => res,
                    Err(error) => {
                        tracing::error!(?error, "failed to read response from server");
                        break;
                    }
                };

                let mut lock = read_handlers.lock().await;
                let Some(handlers) = lock.as_mut() else { break };

                let Some(handler) = handlers.remove(response.id) else {
                    tracing::error!("server sent response for request we did not make");
                    continue;
                };

                let _ = handler.send(response.msg);
            }
            read_cancel.cancel();
        });

        let (to_send_tx, mut to_send_rx) = mpsc::channel::<WireMessage<S::Request>>(1024);

        let write_cancel = cancel.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            while let Some(Some(msg)) = write_cancel.run_until_cancelled(to_send_rx.recv()).await {
                buf.clear();
                serde_json::to_writer(&mut buf, &msg).expect("failed to serialize request");
                buf.push(b'\n');
                if let Err(error) = write.write_all(&buf).await {
                    tracing::error!(?error, "failed to request to server");
                    break;
                }
                let _ = write.flush().await;
            }
            write_cancel.cancel();
        });

        let handlers_kill = handlers.clone();
        let cancel_kill = cancel.clone();
        tokio::spawn(async move {
            cancel_kill.cancelled().await;

            tracing::info!("closing all active handlers");
            let mut handlers = handlers_kill.lock().await;
            let _ = handlers.take();
        });

        Ok(Self {
            handlers,
            to_send: to_send_tx,
            cancel,
            _phantom: PhantomData,
        })
    }

    pub async fn send_request(
        &self,
        request: S::Request,
    ) -> Result<S::Response, ServiceClientError> {
        match self.send(WrappedMsg::Msg(request)).await? {
            WrappedMsg::FailedToParseMessage => Err(ServiceClientError::RequestNotSupported),
            WrappedMsg::Msg(msg) => Ok(msg),
            _ => Err(ServiceClientError::UnexpectedResponse),
        }
    }

    pub async fn ping(&self) -> Result<(), ServiceClientError> {
        match self.send(WrappedMsg::Ping).await? {
            WrappedMsg::Ping => Ok(()),
            _ => Err(ServiceClientError::UnexpectedResponse),
        }
    }

    async fn send(
        &self,
        request: WrappedMsg<S::Request>,
    ) -> Result<WrappedMsg<S::Response>, ServiceClientError> {
        let (id, rx) = {
            let mut lock = self.handlers.lock().await;
            let Some(handlers) = lock.as_mut() else {
                return Err(ServiceClientError::ServerDisconnected);
            };

            let entry = handlers
                .vacant_entry()
                .ok_or(ServiceClientError::TooManyReqeusts)?;

            let (tx, rx) = oneshot::channel();
            (entry.insert(tx), rx)
        };

        self.to_send
            .send(WireMessage { id, msg: request })
            .await
            .map_err(|_| ServiceClientError::ServerDisconnected)?;

        let response = rx
            .await
            .map_err(|_| ServiceClientError::ServerDisconnected)?;

        Ok(response)
    }
}

impl<S: ServiceProto> Drop for ServiceClient<S> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServiceClientError {
    RequestNotSupported,
    ServerDisconnected,
    TooManyReqeusts,
    UnexpectedResponse,
}

impl<S: ServiceProto> ServiceServer<S> {
    pub async fn start() -> Result<Receiver<ServiceRequest<S>>, std::io::Error> {
        let endpont = match Endpoint::new(ServerId::new(S::socket_id()), tipsy::OnConflict::Error) {
            Ok(res) => res,
            Err(error) => {
                tracing::error!(
                    ?error,
                    "got error setting up server, checking if already exists"
                );

                'try_connect: {
                    let Ok(client) = ServiceClient::<S>::connect().await else {
                        break 'try_connect;
                    };

                    if client.ping().await.is_ok() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            "server already running",
                        ));
                    }
                }

                Endpoint::new(ServerId::new(S::socket_id()), tipsy::OnConflict::Overwrite)?
            }
        };

        let (request_tx, request_rx) = mpsc::channel::<ServiceRequest<S>>(1024);

        let server_cancel = CancellationToken::new();

        let mut connections = endpont.incoming()?;
        tokio::spawn(async move {
            while let Some(Some(conn)) = server_cancel.run_until_cancelled(connections.next()).await
            {
                let stream = match conn {
                    Ok(conn) => conn,
                    Err(error) => {
                        tracing::error!(?error, "failed to receive next connection");
                        break;
                    }
                };

                tracing::info!("Got new socket connection");
                let request_tx = request_tx.clone();
                let server_cancel = server_cancel.clone();
                let client_cancel = server_cancel.child_token();

                let (read, mut write) = tokio::io::split(stream);
                let (send_to_client, mut to_send) = mpsc::channel::<WireMessage<S::Response>>(1024);

                let write_cancel = client_cancel.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();

                    while let Some(Some(msg)) =
                        write_cancel.run_until_cancelled(to_send.recv()).await
                    {
                        buf.clear();
                        serde_json::to_writer(&mut buf, &msg).expect("failed to serialize json");
                        buf.push(b'\n');

                        if let Err(error) = write.write_all(&buf).await {
                            tracing::error!(?error, "failed to write message to client");
                            break;
                        }
                        let _ = write.flush().await;
                    }

                    write_cancel.cancel();
                });

                tokio::spawn(client_cancel.clone().run_until_cancelled_owned(async move {
                    let mut reader = tokio::io::BufReader::new(read).lines();

                    loop {
                        let line = match reader.next_line().await {
                            Err(error) => {
                                tracing::error!(?error, "error reading next line from client");
                                break;
                            }
                            Ok(None) => {
                                tracing::info!("client closed");
                                break;
                            }
                            Ok(Some(line)) => line,
                        };

                        let request =
                            match serde_json::from_str::<WireRequestRecoverable<S::Request>>(&line)
                            {
                                Ok(request) => request,
                                Err(error) => {
                                    tracing::error!(?error, "failed to parse request from client");
                                    break;
                                }
                            };

                        match request {
                            WireRequestRecoverable::Request(WireMessage {
                                id,
                                msg: WrappedMsg::Msg(msg),
                            }) => {
                                if request_tx
                                    .send(ServiceRequest {
                                        msg,
                                        sender: ResponseSender {
                                            id,
                                            response: send_to_client.clone(),
                                        },
                                    })
                                    .await
                                    .is_err()
                                {
                                    tracing::info!("request listener is closed");
                                    server_cancel.cancel();
                                    return;
                                }
                            }
                            WireRequestRecoverable::Request(WireMessage {
                                id,
                                msg: WrappedMsg::Ping,
                            }) => {
                                let _ = send_to_client
                                    .send(WireMessage {
                                        id,
                                        msg: WrappedMsg::Pong,
                                    })
                                    .await;
                            }
                            WireRequestRecoverable::Failed { id } => {
                                let _ = send_to_client
                                    .send(WireMessage {
                                        id,
                                        msg: WrappedMsg::FailedToParseMessage,
                                    })
                                    .await;
                            }
                            _ => {}
                        }
                    }

                    client_cancel.cancel();
                }));
            }

            server_cancel.cancel();
            tracing::info!("server is shut down");
        });

        Ok(request_rx)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "details")]
pub enum ServiceRequestMessage {
    CommVersion,
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ServiceStatus {
    ShuttingDown,
    NotSetup,
    Running,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum WireRequestRecoverable<T> {
    Request(WireMessage<T>),
    Failed { id: u64 },
}

#[derive(Debug, Serialize, Deserialize)]
enum WrappedMsg<T> {
    FailedToParseMessage,
    Ping,
    Pong,
    Msg(T),
}

#[derive(Debug, Serialize, Deserialize)]
struct WireMessage<T> {
    id: u64,
    msg: WrappedMsg<T>,
}

pub struct ServiceRequest<S: ServiceProto> {
    pub msg: S::Request,
    pub sender: ResponseSender<S::Response>,
}

pub struct ResponseSender<T> {
    id: u64,
    response: Sender<WireMessage<T>>,
}

impl<T> ResponseSender<T> {
    pub async fn send(self, response: T) -> bool {
        #[cfg_attr(any(), rustfmt::skip)]
        self.response.send(WireMessage {
            id: self.id,
            msg: WrappedMsg::Msg(response),
        }).await.is_ok()
    }
}

#[cfg(test)]
mod test {
    use serde::{Deserialize, Serialize};
    use tokio_util::sync::CancellationToken;

    use crate::service::client_server::{
        ServiceClient, ServiceClientError, ServiceProto, ServiceRequest, ServiceServer,
    };

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    enum Request {
        Add(u32, u32),
        Mul(u32, u32),
    }

    #[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
    struct Response(u64);

    struct Proto;
    impl ServiceProto for Proto {
        type Request = Request;
        type Response = Response;
        fn socket_id() -> &'static str {
            "test-socket"
        }
    }

    #[tokio::test]
    async fn client_server_test() {
        let _ = tracing_subscriber::fmt::try_init();

        let mut server = ServiceServer::<Proto>::start().await.unwrap();
        let client = ServiceClient::<Proto>::connect().await.unwrap();

        let end = CancellationToken::new();
        tokio::spawn(end.clone().run_until_cancelled_owned(async move {
            while let Some(ServiceRequest { msg, sender }) = server.recv().await {
                match msg {
                    Request::Add(a, b) => sender.send(Response(a as u64 + b as u64)).await,
                    Request::Mul(a, b) => sender.send(Response(a as u64 * b as u64)).await,
                };
            }
        }));

        assert_eq!(
            client.send_request(Request::Add(1, 2)).await,
            Ok(Response(3))
        );
        assert_eq!(
            client.send_request(Request::Mul(3, 4)).await,
            Ok(Response(12))
        );

        end.cancel();
        assert_eq!(
            client.send_request(Request::Mul(2, 3)).await,
            Err(ServiceClientError::ServerDisconnected)
        );
    }
}
