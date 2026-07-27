use std::{collections::HashSet, net::SocketAddr, time::Duration};

use message_encoding::MessageEncoding;
use playit_agent_proto::{
    control_feed::ControlFeed,
    control_messages::{ControlRequest, ControlResponse, Ping},
    rpc::ControlRpcMessage,
};

use crate::utils::now_milli;
use tokio::time::Instant;

use super::{PacketIO, connected_control::ConnectedControl, errors::SetupError};

pub struct AddressSelector<IO: PacketIO> {
    options: Vec<SocketAddr>,
    packet_io: IO,
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, Ipv6Addr, SocketAddr},
        sync::atomic::{AtomicBool, Ordering},
    };

    use message_encoding::MessageEncoding;
    use playit_agent_proto::{
        control_feed::ControlFeed,
        control_messages::{ControlResponse, Pong},
        rpc::ControlRpcMessage,
    };

    use super::AddressSelector;
    use crate::agent_control::PacketIO;

    struct MockPacketIo {
        response_addr: SocketAddr,
        response: Vec<u8>,
        response_ready: AtomicBool,
    }

    impl PacketIO for MockPacketIo {
        async fn send_to(&self, buf: &[u8], target: SocketAddr) -> std::io::Result<usize> {
            if target == self.response_addr {
                self.response_ready.store(true, Ordering::Release);
                Ok(buf.len())
            } else {
                Err(std::io::Error::from(std::io::ErrorKind::NetworkUnreachable))
            }
        }

        async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
            while !self.response_ready.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
            buf[..self.response.len()].copy_from_slice(&self.response);
            Ok((self.response.len(), self.response_addr))
        }
    }

    #[tokio::test]
    async fn unreachable_ipv6_does_not_delay_working_ipv4() {
        let ipv4 = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 5525);
        let ipv6 = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 5525);
        let pong = Pong {
            request_now: 1,
            server_now: 1,
            server_id: 1,
            data_center_id: 1,
            client_addr: "192.0.2.1:1000".parse().unwrap(),
            tunnel_addr: ipv4,
            session_expire_at: None,
        };
        let mut response = Vec::new();
        ControlFeed::Response(ControlRpcMessage {
            request_id: 1,
            content: ControlResponse::Pong(pong),
        })
        .write_to(&mut response)
        .unwrap();
        let io = MockPacketIo {
            response_addr: ipv4,
            response,
            response_ready: AtomicBool::new(false),
        };

        let connected = AddressSelector::new(vec![ipv6, ipv4], io)
            .connect_to_first()
            .await
            .unwrap();

        assert_eq!(connected.control_addr(), ipv4);
    }
}

impl<IO: PacketIO> AddressSelector<IO> {
    pub fn new(options: Vec<SocketAddr>, packet_io: IO) -> Self {
        AddressSelector { options, packet_io }
    }

    pub async fn connect_to_first(self) -> Result<ConnectedControl<IO>, SetupError> {
        let mut buffer: Vec<u8> = Vec::new();
        let options: HashSet<_> = self.options.into_iter().collect();

        for attempt in 1..=3 {
            let mut sent_to = HashSet::new();

            for &addr in &options {
                tracing::debug!(%addr, attempt, "Trying tunnel server");
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
                    tracing::debug!(?error, %addr, attempt, "Failed to send initial ping");
                    continue;
                }
                sent_to.insert(addr);
            }

            let deadline = Instant::now() + Duration::from_millis(750);
            while !sent_to.is_empty() {
                buffer.resize(2048, 0);
                let res =
                    tokio::time::timeout_at(deadline.into(), self.packet_io.recv_from(&mut buffer))
                        .await;

                match res {
                    Ok(Ok((bytes, peer))) => {
                        if !sent_to.contains(&peer) {
                            tracing::debug!(%peer, "Ignoring response from an untried address");
                            continue;
                        }

                        let mut reader = &buffer[..bytes];
                        match ControlFeed::read_from(&mut reader) {
                            Ok(ControlFeed::Response(msg)) if msg.request_id == 1 => {
                                match msg.content {
                                    ControlResponse::Pong(pong) => {
                                        tracing::debug!(%peer, ?pong, "Received initial pong");
                                        return Ok(ConnectedControl::new(
                                            peer,
                                            self.packet_io,
                                            pong,
                                        ));
                                    }
                                    other => tracing::debug!(
                                        %peer,
                                        ?other,
                                        "Ignoring non-pong response"
                                    ),
                                }
                            }
                            Ok(other) => {
                                tracing::debug!(%peer, ?other, "Ignoring unexpected control feed")
                            }
                            Err(error) => {
                                tracing::debug!(%peer, ?error, "Failed to parse response data")
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::debug!(?error, attempt, "Failed to receive UDP packet");
                        break;
                    }
                    Err(_) => break,
                }
            }

            tracing::debug!(attempt, "No tunnel server responded");
        }

        tracing::debug!(addresses = ?options, "All tunnel server connection attempts failed");
        Err(SetupError::FailedToConnect)
    }
}
