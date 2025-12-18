use std::{net::SocketAddr, time::Duration};

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
            self.closed = self.end.as_mut().unwrap().try_recv().is_ok();
        }
        self.closed
    }

    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        self.end.take().unwrap().await.unwrap();
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
            let mut packet = loop {
                if let Some(packet) = self.packets.allocate() {
                    break packet;
                }

                tokio::select! {
                    _ = self.cancel.cancelled() => break 'rx_loop,
                    _ = tokio::time::sleep(Duration::from_millis(30)) => continue,
                }
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
                Err(error) => {
                    tracing::error!(?error, id = self.id, "failed to receive UDP packet");
                    break;
                }
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
