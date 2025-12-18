use std::{net::SocketAddr, time::Duration};

use message_encoding::MessageEncoding;
use playit_agent_proto::{
    control_feed::ControlFeed,
    control_messages::{ControlRequest, ControlResponse, Ping},
    rpc::ControlRpcMessage,
};

use crate::utils::now_milli;

use super::{connected_control::ConnectedControl, errors::SetupError, PacketIO};

pub struct AddressSelector<IO: PacketIO> {
    options: Vec<SocketAddr>,
    packet_io: IO,
}

impl<IO: PacketIO> AddressSelector<IO> {
    pub fn new(options: Vec<SocketAddr>, packet_io: IO) -> Self {
        AddressSelector { options, packet_io }
    }

    pub async fn connect_to_first(self) -> Result<ConnectedControl<IO>, SetupError> {
        let mut buffer: Vec<u8> = Vec::new();

        for addr in self.options {
            tracing::info!(?addr, "trying to establish tunnel connection");

            let is_ip6 = addr.is_ipv6();
            let attempts = if is_ip6 { 1 } else { 3 };

            for _ in 0..attempts {
                buffer.clear();

                ControlRpcMessage {
                    request_id: 1,
                    content: ControlRequest::Ping(Ping {
                        now: now_milli(),
                        current_ping: None,
                        session_id: None,
                    }),
                }
                .write_to(&mut buffer)?;

                if let Err(error) = self.packet_io.send_to(&buffer, addr).await {
                    tracing::error!(?error, ?addr, "failed to send initial ping");
                    break;
                }

                buffer.resize(2048, 0);

                let waits = if is_ip6 { 3 } else { 5 };
                for i in 0..waits {
                    let res = tokio::time::timeout(
                        Duration::from_millis(500),
                        self.packet_io.recv_from(&mut buffer),
                    )
                    .await;

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
                                        tracing::error!(
                                            ?msg,
                                            "got response with unexpected request_id"
                                        );
                                        continue;
                                    }

                                    match msg.content {
                                        ControlResponse::Pong(pong) => {
                                            tracing::info!(
                                                ?pong,
                                                "got initial pong from tunnel server"
                                            );
                                            return Ok(ConnectedControl::new(
                                                addr,
                                                self.packet_io,
                                                pong,
                                            ));
                                        }
                                        other => {
                                            tracing::error!(
                                                ?other,
                                                "expected pong got other response"
                                            );
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
                            tracing::warn!(%addr, "waited {}ms for pong", (i + 1) * 500);
                        }
                    }
                }

                tracing::error!("timeout waiting for pong");
            }

            tracing::error!("failed to ping tunnel server");
        }

        Err(SetupError::FailedToConnect)
    }
}
