use std::{net::SocketAddr, time::Duration};

use message_encoding::MessageEncoding;
use playit_agent_proto::{
    control_feed::ControlFeed,
    control_messages::{ControlRequest, ControlResponse, Ping},
    rpc::ControlRpcMessage,
};

use crate::utils::now_milli;

use super::{PacketIO, connected_control::ConnectedControl, errors::SetupError};

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
            tracing::debug!(%addr, "probing tunnel control address");

            let is_ip6 = addr.is_ipv6();
            let attempts = if is_ip6 { 1 } else { 3 };

            for attempt in 1..=attempts {
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
                    tracing::warn!(?error, %addr, "could not send probe ping to tunnel address");
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
                                tracing::debug!(
                                    %peer,
                                    %addr,
                                    "ignoring probe response from unexpected source"
                                );
                                continue;
                            }

                            let mut reader = &buffer[..bytes];
                            match ControlFeed::read_from(&mut reader) {
                                Ok(ControlFeed::Response(msg)) => {
                                    if msg.request_id != 1 {
                                        tracing::debug!(
                                            ?msg,
                                            "ignoring tunnel response for a different request_id"
                                        );
                                        continue;
                                    }

                                    match msg.content {
                                        ControlResponse::Pong(pong) => {
                                            tracing::debug!(
                                                %addr,
                                                ?pong,
                                                "received initial pong from tunnel server"
                                            );
                                            return Ok(ConnectedControl::new(
                                                addr,
                                                self.packet_io,
                                                pong,
                                            ));
                                        }
                                        other => {
                                            tracing::debug!(
                                                %addr,
                                                ?other,
                                                "expected pong, got a different control response"
                                            );
                                        }
                                    }
                                }
                                Ok(other) => {
                                    tracing::debug!(
                                        %addr,
                                        ?other,
                                        "expected control response, got something else"
                                    );
                                }
                                Err(error) => {
                                    tracing::debug!(
                                        ?error,
                                        %addr,
                                        "could not parse tunnel control response"
                                    );
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            tracing::debug!(
                                ?error,
                                %addr,
                                "udp recv failed while waiting for probe pong"
                            );
                        }
                        Err(_) => {
                            tracing::trace!(
                                %addr,
                                waited_ms = (i + 1) * 500,
                                "still waiting for pong from tunnel server"
                            );
                        }
                    }
                }

                tracing::debug!(
                    %addr,
                    attempt,
                    attempts,
                    "no pong from tunnel server within timeout"
                );
            }

            tracing::warn!(%addr, "failed to reach tunnel server after all attempts");
        }

        Err(SetupError::FailedToConnect)
    }
}
