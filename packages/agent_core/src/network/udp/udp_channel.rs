use std::{io, time::Duration};

use playit_agent_proto::{
    control_messages::UdpChannelDetails,
    udp_proto::{UdpDatagram, UdpFlow},
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

use super::{
    packets::{Packet, Packets},
    udp_errors::udp_errors,
};
use crate::agent_control::{DualStackUdpSocket, PacketIO};

pub struct UdpChannel {
    commands: mpsc::Sender<Command>,
    recv: mpsc::Receiver<(UdpFlow, Packet)>,
    status: watch::Receiver<Status>,
    task: JoinHandle<()>,
}

enum Command {
    Session(UdpChannelDetails),
    Send(UdpFlow, Packet),
}

#[derive(Clone, Default)]
struct Status {
    established: Option<Instant>,
    establish_sent: Option<Instant>,
}

// One task owns the socket and session. Session replacement and packet validation are ordered.
struct Worker {
    socket: DualStackUdpSocket,
    packets: Packets,
    commands: mpsc::Receiver<Command>,
    output: mpsc::Sender<(UdpFlow, Packet)>,
    status: watch::Sender<Status>,
    session: Option<UdpChannelDetails>,
    next_establish: Instant,
}

impl UdpChannel {
    pub async fn new(packets: Packets) -> io::Result<Self> {
        let socket = DualStackUdpSocket::new().await?;
        let (commands, command_rx) = mpsc::channel(1024);
        let (output, recv) = mpsc::channel(4096);
        let (status_tx, status) = watch::channel(Status::default());
        let task = tokio::spawn(
            Worker {
                socket,
                packets,
                commands: command_rx,
                output,
                status: status_tx,
                session: None,
                next_establish: Instant::now(),
            }
            .run(),
        );
        Ok(Self {
            commands,
            recv,
            status,
            task,
        })
    }

    pub fn time_since_established(&self) -> Option<Duration> {
        self.status.borrow().established.map(|at| at.elapsed())
    }

    pub fn time_since_establish_send(&self) -> Option<Duration> {
        self.status.borrow().establish_sent.map(|at| at.elapsed())
    }

    pub async fn update_session(&self, details: UdpChannelDetails) -> io::Result<()> {
        self.commands
            .send(Command::Session(details))
            .await
            .map_err(|_| io::ErrorKind::BrokenPipe.into())
    }

    pub async fn send(&self, flow: UdpFlow, packet: Packet) -> io::Result<()> {
        self.commands
            .send(Command::Send(flow, packet))
            .await
            .map_err(|_| io::ErrorKind::BrokenPipe.into())
    }

    pub async fn recv(&mut self) -> Option<(UdpFlow, Packet)> {
        self.recv.recv().await
    }
}

impl Drop for UdpChannel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Worker {
    async fn run(mut self) {
        // Receive complete datagrams before applying the pool's payload limit.
        let mut buffer = vec![0; 65_536];
        loop {
            tokio::select! {
                command = self.commands.recv() => match command {
                    None => break,
                    Some(Command::Session(details)) => {
                        if self.session.as_ref() != Some(&details) {
                            self.status.send_replace(Status::default());
                        }
                        self.session = Some(details);
                        self.establish().await;
                    }
                    Some(Command::Send(flow, packet)) => self.send(flow, packet).await,
                },
                _ = tokio::time::sleep_until(self.next_establish), if self.session.is_some() => {
                    self.establish().await;
                }
                result = self.socket.recv_from(&mut buffer) => match result {
                    Ok((len, source)) => {
                        if self.session.as_ref().map(|s| s.tunnel_addr) != Some(source) {
                            udp_errors().recv_source_no_match.inc();
                            continue;
                        }
                        let data = &buffer[..len];
                        let (flow, payload) = match UdpDatagram::decode(data) {
                            Ok(UdpDatagram::Established) => {
                                self.status.send_modify(|status| status.established = Some(Instant::now()));
                                continue;
                            }
                            Ok(UdpDatagram::Data { flow, payload }) => (flow, payload),
                            Err(_) => {
                                udp_errors().recv_invalid_footer_id.inc();
                                continue;
                            }
                        };
                        let Some(mut packet) = self.packets.allocate() else {
                            udp_errors().packet_pool_exhausted.inc();
                            continue;
                        };
                        if packet.set_len(payload.len()).is_err() {
                            udp_errors().packet_too_large.inc();
                            continue;
                        }
                        packet.as_mut().copy_from_slice(payload);
                        // Saturation drops datagrams without blocking session maintenance.
                        if self.output.try_send((flow, packet)).is_err() {
                            udp_errors().packet_queue_full.inc();
                        }
                    }
                    Err(_) => {
                        udp_errors().recv_io_error.inc();
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                },
            }
        }
    }

    async fn establish(&mut self) {
        let Some(session) = &self.session else { return };
        if self
            .socket
            .send_to(&session.token, session.tunnel_addr)
            .await
            .is_err()
        {
            udp_errors().establish_send_io_error.inc();
        } else {
            self.status
                .send_modify(|status| status.establish_sent = Some(Instant::now()));
        }
        let healthy = self
            .status
            .borrow()
            .established
            .is_some_and(|at| at.elapsed() < Duration::from_secs(15));
        self.next_establish = Instant::now() + Duration::from_secs(if healthy { 10 } else { 3 });
    }

    async fn send(&self, flow: UdpFlow, mut packet: Packet) {
        let Some(session) = &self.session else {
            udp_errors().no_session_send_fail.inc();
            return;
        };
        let len = packet.len();
        if !flow.write_to(&mut packet.full_slice_mut()[len..]) {
            udp_errors().tail_append_fail.inc();
            return;
        }
        packet.set_len(len + flow.footer_len()).unwrap();
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

#[cfg(test)]
mod tests {
    use super::*;
    use playit_agent_proto::udp_proto::UDP_CHANNEL_ESTABLISH_ID;
    use std::sync::Arc;
    use tokio::net::UdpSocket;

    #[tokio::test]
    async fn session_reset_and_ack_work_when_packet_pool_is_exhausted() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let pool = Packets::new(1);
        let _held_packet = pool.allocate().unwrap();
        let channel = UdpChannel::new(pool).await.unwrap();
        let mut buffer = [0; 128];
        let details = |token| UdpChannelDetails {
            tunnel_addr: server.local_addr().unwrap(),
            token: Arc::new(vec![token]),
        };
        channel.update_session(details(1)).await.unwrap();
        let (_, addr) = tokio::time::timeout(Duration::from_secs(1), server.recv_from(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        server
            .send_to(&UDP_CHANNEL_ESTABLISH_ID.to_be_bytes(), addr)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while channel.time_since_established().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        channel.update_session(details(2)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), server.recv_from(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert!(channel.time_since_established().is_none());
        let handle = channel.task.abort_handle();
        drop(channel);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !handle.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
