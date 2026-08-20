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
    task: Option<tokio::task::JoinHandle<crate::playit_agent::ServiceExit>>,
}

impl UdpReceiverSetup {
    pub fn spawn<I: PacketRx>(&self, id: u64, rx: I) -> UdpReceiver {
        let cancel = CancellationToken::new();

        let task = tokio::spawn(
            Task {
                id,
                rx,
                packets: self.packets.clone(),
                output: self.output.clone(),
                cancel: cancel.clone(),
            }
            .start(),
        );

        UdpReceiver {
            id,
            cancel,
            task: Some(task),
        }
    }
}

impl UdpReceiver {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn is_closed(&self) -> bool {
        self.task
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }

    pub async fn shutdown(mut self, deadline: Duration) -> crate::playit_agent::ServiceExit {
        self.cancel.cancel();
        let Some(mut task) = self.task.take() else {
            return crate::playit_agent::ServiceExit::Completed;
        };
        match tokio::time::timeout(deadline, &mut task).await {
            Ok(Ok(exit)) => exit,
            Ok(Err(error)) => crate::playit_agent::ServiceExit::Panicked(error.to_string()),
            Err(_) => {
                task.abort();
                let _ = task.await;
                crate::playit_agent::ServiceExit::DeadlineExceeded
            }
        }
    }
}

impl Drop for UdpReceiver {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

struct Task<I: PacketRx> {
    id: u64,
    rx: I,
    packets: Packets,
    cancel: CancellationToken,
    output: Sender<UdpReceivedPacket>,
}

pub struct UdpReceivedPacket {
    pub rx_id: u64,
    pub packet: Packet,
    pub from: SocketAddr,
}

impl<I: PacketRx> Task<I> {
    async fn start(self) -> crate::playit_agent::ServiceExit {
        let mut next_error_log_allowed = tokio::time::Instant::now();
        let mut consecutive_errors = 0u8;

        loop {
            let mut packet = tokio::select! {
                _ = self.cancel.cancelled() => return crate::playit_agent::ServiceExit::Cancelled,
                p = self.packets.allocate_wait() => p,
            };

            let res = tokio::select! {
                _ = self.cancel.cancelled() => return crate::playit_agent::ServiceExit::Cancelled,
                res = self.rx.recv_from(packet.full_slice_mut()) => res,
            };

            let packet = match res {
                Ok((bytes, source)) => {
                    consecutive_errors = 0;
                    if let Err(error) = packet.set_len(bytes) {
                        return crate::playit_agent::ServiceExit::Io {
                            operation: "resize origin UDP packet".to_owned(),
                            message: error.to_string(),
                        };
                    }

                    UdpReceivedPacket {
                        rx_id: self.id,
                        packet,
                        from: source,
                    }
                }
                Err(error) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    let now = tokio::time::Instant::now();
                    if next_error_log_allowed <= now {
                        tracing::warn!(?error, id = self.id, "failed to receive UDP packet");
                        next_error_log_allowed = now + Duration::from_secs(1);
                    }

                    if consecutive_errors >= 8 {
                        return crate::playit_agent::ServiceExit::Io {
                            operation: "receive origin UDP packet".to_owned(),
                            message: error.to_string(),
                        };
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
            };

            let result = self
                .cancel
                .run_until_cancelled(self.output.send(packet))
                .await;
            match result {
                Some(Ok(_)) => {}
                None => return crate::playit_agent::ServiceExit::Cancelled,
                Some(Err(_)) => {
                    return crate::playit_agent::ServiceExit::QueueClosed("udp origin".to_owned());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};

    struct FailedReceiver;

    impl PacketRx for FailedReceiver {
        async fn recv_from(&self, _buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "fixture socket failure",
            ))
        }
    }

    struct OnePacketReceiver;

    impl PacketRx for OnePacketReceiver {
        async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
            buf[0] = 1;
            Ok((1, SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1234).into()))
        }
    }

    async fn wait_until_closed(receiver: &UdpReceiver) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !receiver.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("UDP receiver stopped");
    }

    #[tokio::test]
    async fn repeated_socket_failure_reaches_the_owner() {
        let (output, _input) = tokio::sync::mpsc::channel(1);
        let setup = UdpReceiverSetup {
            packets: Packets::new(1),
            output,
        };
        let receiver = setup.spawn(1, FailedReceiver);
        wait_until_closed(&receiver).await;
        assert!(matches!(
            receiver.shutdown(Duration::from_secs(1)).await,
            crate::playit_agent::ServiceExit::Io { operation, .. }
                if operation == "receive origin UDP packet"
        ));
    }

    #[tokio::test]
    async fn closed_origin_queue_reaches_the_owner() {
        let (output, input) = tokio::sync::mpsc::channel(1);
        drop(input);
        let setup = UdpReceiverSetup {
            packets: Packets::new(1),
            output,
        };
        let receiver = setup.spawn(1, OnePacketReceiver);
        wait_until_closed(&receiver).await;
        assert_eq!(
            receiver.shutdown(Duration::from_secs(1)).await,
            crate::playit_agent::ServiceExit::QueueClosed("udp origin".to_owned())
        );
    }
}
