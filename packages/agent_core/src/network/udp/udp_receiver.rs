use super::{
    packets::{Packet, Packets},
    udp_errors::udp_errors,
};
use crate::agent_control::PacketRx;
use std::{net::SocketAddr, time::Duration};
use tokio::{sync::mpsc::Sender, task::JoinHandle};

pub struct UdpReceiverSetup {
    pub packets: Packets,
    pub output: Sender<UdpReceivedPacket>,
}

pub struct UdpReceiver {
    id: u64,
    task: JoinHandle<()>,
}

impl UdpReceiverSetup {
    pub fn create<I: PacketRx>(&self, id: u64, rx: I) -> UdpReceiver {
        let packets = self.packets.clone();
        let output = self.output.clone();
        let task = tokio::spawn(async move {
            let mut buffer = vec![0; super::packets::PACKET_LEN + 1];
            loop {
                let (len, from) = match rx.recv_from(&mut buffer).await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::debug!(?error, id, "UDP origin receive failed");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                };
                let Some(mut packet) = packets.allocate() else {
                    udp_errors().packet_pool_exhausted.inc();
                    continue;
                };
                if packet.set_len(len).is_err() {
                    udp_errors().packet_too_large.inc();
                    continue;
                }
                packet.as_mut().copy_from_slice(&buffer[..len]);
                match output.try_send(UdpReceivedPacket {
                    rx_id: id,
                    packet,
                    from,
                }) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        udp_errors().packet_queue_full.inc()
                    }
                }
            }
        });
        UdpReceiver { id, task }
    }
}

impl UdpReceiver {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn is_closed(&self) -> bool {
        self.task.is_finished()
    }
    pub async fn shutdown(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for UdpReceiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct UdpReceivedPacket {
    pub rx_id: u64,
    pub packet: Packet,
    pub from: SocketAddr,
}
