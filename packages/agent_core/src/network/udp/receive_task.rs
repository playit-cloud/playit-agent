use std::{net::SocketAddr, sync::{atomic::{AtomicBool, Ordering}, Arc}, time::Duration};

use tokio::sync::mpsc::{error::SendTimeoutError, Sender};

use crate::{agent_control::PacketRx, utils::error_helper::MaxErrorInterval};

use super::packets::{Packet, Packets};

pub struct UdpReceiverTask<I: PacketRx> {
    pub id: u64,
    pub rx: I,
    pub run: Arc<AtomicBool>,
    pub packets: Packets,
    pub tx: Sender<SocketPacket>,
    pub rx_offset: usize,
}

pub struct SocketPacket {
    pub socket_id: u64,
    pub packet: Packet,
    pub address: SocketAddr,
    pub data_offset: usize,
}

impl<I: PacketRx> UdpReceiverTask<I> {
    pub async fn start(self) {
        let mut rx_packet = None;

        let mut out_of_packets = MaxErrorInterval::new(Duration::from_secs(5));
        let mut io_error = MaxErrorInterval::new(Duration::from_secs(3));

        while self.run.load(Ordering::Relaxed) {
            if rx_packet.is_none() {
                rx_packet = self.packets.allocate();

                if rx_packet.is_none() {
                    if out_of_packets.check() {
                        tracing::warn!("out of free packets to receive from UDP socket");
                    }

                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            }

            let packet = rx_packet.as_mut().expect("rx packet should be allocated");
            let rx_buffer = &mut packet.full_slice_mut()[self.rx_offset..];

            match tokio::time::timeout(Duration::from_secs(5), self.rx.recv_from(rx_buffer)).await {
                Err(_) => {
                    continue;
                }
                Ok(Err(error)) => {
                    if io_error.check() {
                        tracing::error!(?error, "failed to receive UDP packet");
                    }
                }
                Ok(Ok((bytes, source))) => {
                    packet.set_len(bytes + self.rx_offset).expect("receive length too large");

                    let send_res = self.tx.send_timeout(SocketPacket {
                        socket_id: self.id,
                        packet: rx_packet.take().expect("missing rx packet"),
                        address: source,
                        data_offset: self.rx_offset,
                    }, Duration::from_secs(1)).await;

                    match send_res {
                        Ok(()) => {},
                        Err(SendTimeoutError::Closed(_)) => {
                            tracing::error!("UDP receive queue closed, closing receiver");
                            break;
                        }
                        Err(SendTimeoutError::Timeout(_)) => {
                            tracing::error!("timeout sending to UDP receive queue");
                            continue;
                        }
                    }
                },
            }
        }
    }
}
