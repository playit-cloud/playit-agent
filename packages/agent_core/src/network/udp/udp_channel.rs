use std::{
    net::SocketAddr,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

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

pub struct UdpChannelTasks {
    send: SendTask,
    recv: RecvTask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpChannelError {
    SessionQueueClosed,
    SendQueueClosed,
    ReceiveQueueClosed,
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
    pub async fn new(packets: Packets) -> Result<(Self, UdpChannelTasks), std::io::Error> {
        let socket = Arc::new(DualStackUdpSocket::new().await?);

        let (session_tx, session_rx) = channel(32);

        let (send_tx, send_rx) = channel(1024);
        let (recv_tx, recv_rx) = channel(4096);

        let shared = Arc::new(Shared::default());

        let send_task = SendTask {
            socket: socket.clone(),
            session: None,
            session_rx,
            send_rx,
            shared: shared.clone(),
        };
        let recv_task = RecvTask {
            socket,
            packets,
            recv_tx,
            shared: shared.clone(),
        };

        Ok((
            UdpChannel {
                session_tx,
                send: send_tx,
                recv: recv_rx,
                shared,
            },
            UdpChannelTasks {
                send: send_task,
                recv: recv_task,
            },
        ))
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

    pub async fn update_session(&self, details: UdpChannelDetails) -> Result<(), UdpChannelError> {
        self.session_tx
            .send(details)
            .await
            .map_err(|_| UdpChannelError::SessionQueueClosed)
    }

    pub async fn send(&self, flow: UdpFlow, packet: Packet) -> Result<(), UdpChannelError> {
        self.send
            .send((flow, packet))
            .await
            .map_err(|_| UdpChannelError::SendQueueClosed)
    }

    pub async fn recv(&mut self) -> Result<(UdpFlow, Packet), UdpChannelError> {
        self.recv
            .recv()
            .await
            .ok_or(UdpChannelError::ReceiveQueueClosed)
    }
}

impl UdpChannelTasks {
    pub async fn run(
        self,
        cancel: CancellationToken,
    ) -> (
        crate::playit_agent::EngineService,
        crate::playit_agent::ServiceExit,
    ) {
        let mut tasks = JoinSet::new();
        let send_cancel = cancel.child_token();
        tasks.spawn(async move {
            (
                crate::playit_agent::EngineService::UdpChannelSend,
                self.send.start(send_cancel).await,
            )
        });
        let recv_cancel = cancel.child_token();
        tasks.spawn(async move {
            (
                crate::playit_agent::EngineService::UdpChannelRecv,
                self.recv.start(recv_cancel).await,
            )
        });

        tokio::select! {
            _ = cancel.cancelled() => {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                (
                    crate::playit_agent::EngineService::UdpChannelRecv,
                    crate::playit_agent::ServiceExit::Cancelled,
                )
            }
            result = tasks.join_next() => {
                cancel.cancel();
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                match result {
                    Some(Ok(exit)) => exit,
                    Some(Err(error)) => (
                        crate::playit_agent::EngineService::UdpChannelRecv,
                        crate::playit_agent::ServiceExit::Panicked(error.to_string()),
                    ),
                    None => (
                        crate::playit_agent::EngineService::UdpChannelRecv,
                        crate::playit_agent::ServiceExit::Completed,
                    ),
                }
            }
        }
    }
}

impl SendTask {
    async fn start(mut self, cancel: CancellationToken) -> crate::playit_agent::ServiceExit {
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
                _ = cancel.cancelled() => return crate::playit_agent::ServiceExit::Cancelled,
                _ = tokio::time::sleep_until(next_send) => {
                    last_establish_send = Instant::now();
                    self.send_establish().await;
                    continue;
                }
                session_res = self.session_rx.recv() => {
                    let Some(details) = session_res else {
                        return crate::playit_agent::ServiceExit::QueueClosed("udp session".to_owned());
                    };
                    self.handle_session(details).await;
                    continue;
                }
                to_send_res = self.send_rx.recv() => {
                    let Some((flow, to_send)) = to_send_res else {
                        return crate::playit_agent::ServiceExit::QueueClosed("udp send".to_owned());
                    };
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
    async fn start(self, cancel: CancellationToken) -> crate::playit_agent::ServiceExit {
        let mut packet = tokio::select! {
            _ = cancel.cancelled() => return crate::playit_agent::ServiceExit::Cancelled,
            packet = self.packets.allocate_wait() => packet,
        };

        loop {
            let recv = tokio::select! {
                _ = cancel.cancelled() => return crate::playit_agent::ServiceExit::Cancelled,
                recv = self.socket.recv_from(packet.full_slice_mut()) => recv,
            };
            let (bytes, source) = match recv {
                Ok(received) => received,
                Err(error) => {
                    udp_errors().recv_io_error.inc();
                    return crate::playit_agent::ServiceExit::Io {
                        operation: "receive tunnel UDP packet".to_owned(),
                        message: error.to_string(),
                    };
                }
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
                return crate::playit_agent::ServiceExit::QueueClosed("udp receive".to_owned());
            }
            packet = tokio::select! {
                _ = cancel.cancelled() => return crate::playit_agent::ServiceExit::Cancelled,
                packet = self.packets.allocate_wait() => packet,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closed_service_queues_return_errors() {
        let (mut channel, tasks) = UdpChannel::new(Packets::new(1))
            .await
            .expect("bind UDP channel");
        drop(tasks);

        let details = UdpChannelDetails {
            tunnel_addr: "127.0.0.1:1".parse().expect("fixture address"),
            token: Arc::new(vec![1]),
        };
        assert_eq!(
            channel.update_session(details).await,
            Err(UdpChannelError::SessionQueueClosed)
        );
        assert!(matches!(
            channel.recv().await,
            Err(UdpChannelError::ReceiveQueueClosed)
        ));
    }

    #[tokio::test]
    async fn cancellation_joins_both_channel_tasks() {
        let (_channel, tasks) = UdpChannel::new(Packets::new(1))
            .await
            .expect("bind UDP channel");
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(tasks.run(task_cancel));
        cancel.cancel();
        let (_, exit) = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("UDP channel stopped before its deadline")
            .expect("UDP channel task did not panic");
        assert_eq!(exit, crate::playit_agent::ServiceExit::Cancelled);
    }
}
