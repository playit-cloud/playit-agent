use std::{
    net::SocketAddr,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

use playit_agent_proto::{
    control_messages::UdpChannelDetails,
    udp_proto::{UDP_CHANNEL_ESTABLISH_ID, UdpFlow},
};
use tokio::sync::mpsc::{Receiver, Sender, channel};

use crate::{
    agent_control::{DualStackUdpSocket, PacketIO},
    utils::now_milli,
};

use super::{
    packets::{Packet, Packets},
    udp_errors::udp_errors,
};

pub struct UdpChannel {
    session_tx: Sender<UdpChannelDetails>,
    send: Sender<(UdpFlow, Packet)>,
    recv: Receiver<(UdpFlow, Packet)>,
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    establish_rx_epoch: AtomicU64,
    establish_tx_epoch: AtomicU64,
    session_tunnel_addr: RwLock<Option<SocketAddr>>,
}

struct SendTask {
    socket: Arc<DualStackUdpSocket>,
    session: Option<UdpChannelDetails>,
    session_rx: Receiver<UdpChannelDetails>,
    send_rx: Receiver<(UdpFlow, Packet)>,
    shared: Arc<Shared>,
}

struct RecvTask {
    socket: Arc<DualStackUdpSocket>,
    packets: Packets,
    recv_tx: Sender<(UdpFlow, Packet)>,
    shared: Arc<Shared>,
}

impl UdpChannel {
    pub async fn new(packets: Packets) -> Result<Self, std::io::Error> {
        let socket = Arc::new(DualStackUdpSocket::new().await?);

        let (session_tx, session_rx) = channel(32);

        let (send_tx, send_rx) = channel(1024);
        let (recv_tx, recv_rx) = channel(4096);

        let shared = Arc::new(Shared::default());

        tokio::spawn(
            SendTask {
                socket: socket.clone(),
                session: None,
                session_rx,
                send_rx,
                shared: shared.clone(),
            }
            .start(),
        );
        tokio::spawn(
            RecvTask {
                socket,
                packets,
                recv_tx,
                shared: shared.clone(),
            }
            .start(),
        );

        Ok(UdpChannel {
            session_tx,
            send: send_tx,
            recv: recv_rx,
            shared,
        })
    }

    pub fn time_since_established(&self) -> Option<Duration> {
        let ts = self.shared.establish_rx_epoch.load(Ordering::Acquire);
        if ts == 0 {
            return None;
        }
        let now = now_milli();
        Some(Duration::from_millis(now.max(ts) - ts))
    }

    pub fn time_since_establish_send(&self) -> Option<Duration> {
        let ts = self.shared.establish_tx_epoch.load(Ordering::Acquire);
        if ts == 0 {
            return None;
        }
        let now = now_milli();
        Some(Duration::from_millis(now.max(ts) - ts))
    }

    pub async fn update_session(&self, details: UdpChannelDetails) {
        self.session_tx.send(details).await.expect("task closed");
    }

    pub async fn send(&self, flow: UdpFlow, packet: Packet) {
        if self.send.send((flow, packet)).await.is_err() {
            panic!("UdpChannel task closed");
        }
    }

    pub async fn recv(&mut self) -> (UdpFlow, Packet) {
        self.recv.recv().await.expect("UdpChannel task closed")
    }
}

impl SendTask {
    async fn start(mut self) {
        let mut last_establish_send = Instant::now();

        loop {
            let next_send = if self.session.is_none() {
                Instant::now() + Duration::from_secs(5)
            } else {
                let since_establish = Duration::from_millis({
                    let now = now_milli();
                    let last_rx = self.shared.establish_rx_epoch.load(Ordering::Relaxed);
                    now.max(last_rx) - last_rx
                });

                const MIN_INTERVAL: Duration = Duration::from_secs(3);
                const MAX_ESTABLISH_WAIT: Duration = Duration::from_secs(15);
                const SEND_INTERVAL: Duration = Duration::from_secs(10);

                if MAX_ESTABLISH_WAIT <= since_establish {
                    last_establish_send + MIN_INTERVAL
                } else {
                    last_establish_send + SEND_INTERVAL
                }
            };

            tokio::select! {
                _ = tokio::time::sleep_until(next_send) => {
                    last_establish_send = Instant::now();
                    self.send_establish().await;
                    continue;
                }
                session_res = self.session_rx.recv() => {
                    let Some(details) = session_res else { break };
                    self.handle_session(details).await;
                    continue;
                }
                to_send_res = self.send_rx.recv() => {
                    let Some((flow, to_send)) = to_send_res else { break };
                    self.send(flow, to_send).await;
                    continue;
                }
            };
        }
    }

    async fn handle_session(&mut self, details: UdpChannelDetails) {
        let should_send = match self.session.take() {
            None => true,
            Some(old) => {
                if old != details {
                    true
                } else {
                    5_000
                        < now_milli()
                            .saturating_sub(self.shared.establish_rx_epoch.load(Ordering::Relaxed))
                }
            }
        };

        *self.shared.session_tunnel_addr.write().unwrap() = Some(details.tunnel_addr);
        self.session = Some(details);
        if should_send {
            self.send_establish().await;
        }
    }

    async fn send_establish(&self) {
        let Some(session) = self.session.as_ref() else {
            udp_errors().establish_no_session.inc();
            return;
        };

        self.shared
            .establish_tx_epoch
            .store(now_milli(), Ordering::Release);

        if self
            .socket
            .send_to(&session.token[..], session.tunnel_addr)
            .await
            .is_err()
        {
            udp_errors().establish_send_io_error.inc();
        }
    }

    async fn send(&self, flow: UdpFlow, mut packet: Packet) {
        let Some(session) = self.session.as_ref() else {
            udp_errors().no_session_send_fail.inc();
            return;
        };

        let og_len = packet.len();
        let remaining = &mut packet.full_slice_mut()[og_len..];
        if !flow.write_to(remaining) {
            udp_errors().tail_append_fail.inc();
            return;
        }

        packet
            .set_len(og_len + flow.footer_len())
            .expect("should be able to update packet len");

        if self
            .socket
            .send_to(packet.as_ref(), session.tunnel_addr)
            .await
            .is_err()
        {
            udp_errors().send_io_error.inc();
        }
    }
}

impl RecvTask {
    async fn start(self) {
        let mut packet = self.packets.allocate_wait().await;

        loop {
            let Ok((bytes, source)) = self.socket.recv_from(packet.full_slice_mut()).await else {
                udp_errors().recv_io_error.inc();
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };

            let Some(session_addr) = *self.shared.session_tunnel_addr.read().unwrap() else {
                udp_errors().recv_with_no_session.inc();
                continue;
            };

            if session_addr != source {
                udp_errors().recv_source_no_match.inc();
                continue;
            }

            packet.set_len(bytes).expect("failed to update packet len");
            let flow = match UdpFlow::from_tail(packet.as_ref()) {
                Ok(flow) => flow,
                Err(Some(footer)) if footer == UDP_CHANNEL_ESTABLISH_ID => {
                    self.shared
                        .establish_rx_epoch
                        .store(now_milli(), Ordering::Release);
                    continue;
                }
                Err(id) => {
                    if id.is_none() {
                        udp_errors().recv_too_small.inc();
                    } else {
                        udp_errors().recv_invalid_footer_id.inc();
                    }
                    continue;
                }
            };

            packet
                .set_len(bytes - flow.footer_len())
                .expect("failed to remove udp footer");

            if self.recv_tx.send((flow, packet)).await.is_err() {
                break;
            }
            packet = self.packets.allocate_wait().await;
        }
    }
}
