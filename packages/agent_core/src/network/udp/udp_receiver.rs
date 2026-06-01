use std::{io::ErrorKind, net::SocketAddr, time::Duration};

use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

use crate::agent_control::PacketRx;

use super::packets::{Packet, Packets};

pub struct UdpReceiverSetup {
    pub packets: Packets,
    pub output: Sender<UdpReceivedPacket>,
}

pub struct UdpReceiver {
    id: u64,
    cancel: CancellationToken,
    end: Option<tokio::sync::oneshot::Receiver<()>>,
    closed: bool,
}

impl UdpReceiverSetup {
    pub fn create<I: PacketRx>(&self, id: u64, rx: I) -> UdpReceiver {
        let cancel = CancellationToken::new();
        let (end_tx, end_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(
            Task {
                id,
                rx,
                packets: self.packets.clone(),
                output: self.output.clone(),
                cancel: cancel.clone(),
                end: end_tx,
            }
            .start(),
        );

        UdpReceiver {
            id,
            cancel,
            end: Some(end_rx),
            closed: false,
        }
    }
}

impl UdpReceiver {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn is_closed(&mut self) -> bool {
        if !self.closed {
            self.closed = match self.end.as_mut().unwrap().try_recv() {
                Ok(_) => true,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => true,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => false,
            };
        }
        self.closed
    }

    pub async fn shutdown(mut self) {
        self.cancel.cancel();

        let timedout = tokio::time::timeout(Duration::from_secs(15), self.end.take().unwrap())
            .await
            .is_err();

        if timedout {
            panic!("Timeout waiting for UdpReceiver to shutdown 15s+");
        }

        self.closed = true;
    }
}

impl Drop for UdpReceiver {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Task<I: PacketRx> {
    id: u64,
    rx: I,
    packets: Packets,
    cancel: CancellationToken,
    end: tokio::sync::oneshot::Sender<()>,
    output: Sender<UdpReceivedPacket>,
}

pub struct UdpReceivedPacket {
    pub rx_id: u64,
    pub packet: Packet,
    pub from: SocketAddr,
}

impl<I: PacketRx> Task<I> {
    async fn start(self) {
        'rx_loop: loop {
            let mut packet = tokio::select! {
                _ = self.cancel.cancelled() => break 'rx_loop,
                p = self.packets.allocate_wait() => p,
            };

            let res = tokio::select! {
                _ = self.cancel.cancelled() => break,
                res = self.rx.recv_from(packet.full_slice_mut()) => res,
            };

            let packet = match res {
                Ok((bytes, source)) => {
                    packet.set_len(bytes).unwrap();

                    UdpReceivedPacket {
                        rx_id: self.id,
                        packet,
                        from: source,
                    }
                }
                Err(error) => match error.kind() {
                    ErrorKind::Interrupted
                    | ErrorKind::WouldBlock
                    | ErrorKind::TimedOut
                    | ErrorKind::ConnectionReset => {
                        tracing::warn!(?error, id = self.id, "transient UDP receive error");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                    _ => {
                        tracing::error!(?error, id = self.id, "failed to receive UDP packet");
                        break;
                    }
                },
            };

            let result = self
                .cancel
                .run_until_cancelled(self.output.send(packet))
                .await;
            match result {
                Some(Ok(_)) => {}
                None | Some(Err(_)) => break,
            }
        }

        let _ = self.end.send(());
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        future, io,
        net::{Ipv4Addr, SocketAddr, SocketAddrV4},
        sync::Mutex,
    };

    use tokio::{sync::mpsc, time::Duration};

    use crate::network::udp::packets::Packets;

    use super::*;

    struct MockPacketRx {
        responses: Mutex<VecDeque<io::Result<(Vec<u8>, SocketAddr)>>>,
    }

    impl PacketRx for MockPacketRx {
        fn recv_from(
            &self,
            buf: &mut [u8],
        ) -> impl Future<Output = io::Result<(usize, SocketAddr)>> + Sync + Send {
            let response = self
                .responses
                .lock()
                .expect("responses lock poisoned")
                .pop_front()
                .expect("missing mock response");

            let result = match response {
                Ok((bytes, source)) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok((bytes.len(), source))
                }
                Err(error) => Err(error),
            };

            future::ready(result)
        }
    }

    #[tokio::test]
    async fn receiver_continues_after_connection_reset() {
        let source = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1234));
        let rx = MockPacketRx {
            responses: Mutex::new(VecDeque::from([
                Err(io::Error::from(io::ErrorKind::ConnectionReset)),
                Ok((b"still alive".to_vec(), source)),
            ])),
        };
        let packets = Packets::new(2);
        let (output, mut received) = mpsc::channel(1);
        let setup = UdpReceiverSetup { packets, output };

        let receiver = setup.create(7, rx);
        let packet = tokio::time::timeout(Duration::from_secs(1), received.recv())
            .await
            .expect("timed out waiting for packet")
            .expect("receiver output closed");

        assert_eq!(packet.rx_id, 7);
        assert_eq!(packet.from, source);
        assert_eq!(packet.packet.as_ref(), b"still alive");

        receiver.shutdown().await;
    }
}
