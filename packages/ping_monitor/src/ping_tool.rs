use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use message_encoding::MessageEncoding;
use playit_agent_proto::{control_feed::ControlFeed, control_messages::{ControlRequest, ControlResponse, Ping, Pong}, rpc::ControlRpcMessage};
use playit_api_client::api::PingTarget;
use tokio::net::UdpSocket;

use crate::epoch_milli;

pub struct PlayitPingTool {
    udp6: Option<UdpSocket>,
    udp4: UdpSocket,
}

impl PlayitPingTool {
    pub async fn new() -> Result<Self, std::io::Error> {
        Ok(PlayitPingTool {
            udp6: UdpSocket::bind(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0)).await.ok(),
            udp4: UdpSocket::bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)).await?,
        })
    }

    pub async fn send_ping(&self, id: u64, target: &PingTarget) -> Result<bool, std::io::Error> {
        let udp = if target.ip.is_ipv4() {
            &self.udp4
        } else {
            let Some(udp) = &self.udp6 else { return Ok(false) };
            udp
        };

        let now = epoch_milli();
        let msg = ControlRpcMessage {
            request_id: id,
            content: ControlRequest::Ping(Ping {
                now,
                current_ping: None,
                session_id: None,
            }),
        };

        let mut buffer = Vec::new();
        msg.write_to(&mut buffer).unwrap();

        udp.send_to(&buffer, SocketAddr::new(target.ip, target.port)).await?;
        Ok(true)
    }

    pub async fn read_pong(&self) -> Result<(ControlRpcMessage<Pong>, SocketAddr), std::io::Error> {
        let mut response = Vec::with_capacity(1024);
        response.resize(1024, 0u8);

        loop {
            tokio::task::yield_now().await;

            /* Important: no more asyn after recv_from to ensure timeout doesn't drop data */
            let (bytes, source) = if let Some(udp6) = &self.udp6 {
                let (a, b) = response.split_at_mut(512);

                tokio::select! {
                    res = udp6.recv_from(a) => {
                        let (bytes, source) = res?;
                        (&a[..bytes], source)
                    }
                    res = self.udp4.recv_from(b) => {
                        let (bytes, source) = res?;
                        (&b[..bytes], source)
                    }
                }
            } else {
                let (bytes, source) = self.udp4.recv_from(&mut response).await?;
                (&response[..bytes], source)
            };

            let mut reader = bytes;
            if let Ok(data) = ControlFeed::read_from(&mut reader) {
                let ControlFeed::Response(ControlRpcMessage { request_id, content: ControlResponse::Pong(pong) }) = data else { continue };
                return Ok((ControlRpcMessage { request_id, content: pong }, source));
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::{sync::{atomic::{AtomicBool, Ordering}, Arc}, time::Duration};

    use playit_api_client::api::PingTarget;

    use crate::ping_tool::PlayitPingTool;

    #[tokio::test]
    async fn test_ping() {
        let ping = Arc::new(PlayitPingTool::new().await.unwrap());
        let run = Arc::new(AtomicBool::new(true));

        let send_join = {
            let ping = ping.clone();
            let run = run.clone();

            tokio::spawn(async move {
                while run.load(Ordering::Relaxed) {
                    ping.send_ping(32, &PingTarget { ip: "209.25.140.1".parse().unwrap(), port: 5525 }).await.unwrap();
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            })
        };

        let mut result = None;
        for _ in 0..5 {
            match tokio::time::timeout(Duration::from_secs(1), ping.read_pong()).await {
                Err(_) => continue,
                Ok(Err(error)) => {
                    eprintln!("Error: {:?}", error);
                }
                Ok(Ok(pong)) => {
                    result.replace(pong);
                    break;
                }
            }
        }

        run.store(false, Ordering::Relaxed);
        send_join.await.unwrap();

        let result = result.unwrap();
        println!("Got result: {:?}", result);
    }
}