use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::ops::Add;
use std::sync::Arc;
use std::time::{Duration};

use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use tokio::net::UdpSocket;
use tokio::time::Instant;
use crate::agent_state::AgentState;
use crate::name_lookup::address_lookup;

use crate::now_milli;

pub struct PingTask {
    udp4: Option<UdpSocket>,
    udp6: Option<UdpSocket>,
    config: Arc<AgentState>,
    latest_pings: HashMap<IpAddr, PingStatus>,
    recv_buf: Vec<u8>,
}

#[derive(Debug)]
pub struct PingStatus {
    ping: u64,
    tunnel_server_id: u64,
}

impl PingTask {
    pub fn new(config: Arc<AgentState>) -> Self {
        PingTask {
            udp4: None,
            udp6: None,
            config,
            latest_pings: Default::default(),
            recv_buf: vec![0u8; 256],
        }
    }

    pub async fn run(mut self) {
        let mut last_run = 0;

        loop {
            if let Err(error) = self.update().await {
                tracing::error!(?error, "ping task failed to update");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }

            let target_wait = {
                let config = self.config.agent_config.read().await;
                config.ping_interval
            };

            let now = now_milli();
            let current_wait = now - last_run;
            last_run = now;

            if current_wait > target_wait {
                continue;
            }

            let wait = (target_wait - current_wait).min(30_000);
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }
    }

    async fn update(&mut self) -> std::io::Result<()> {
        self.send_pings().await?;

        let has_v4 = self.udp4.is_some();
        let results = match (self.udp4.take(), self.udp6.take()) {
            (Some(mut v4), Some(mut v6)) => {
                let v4_handle = tokio::spawn(async move {
                    let res = match listen_for_responses(&mut v4, 4_000).await {
                        Ok(v) => v,
                        Err(error) => return Err(error),
                    };
                    Ok((v4, res))
                });

                let v6_handle = tokio::spawn(async move {
                    let res = match listen_for_responses(&mut v6, 4_000).await {
                        Ok(v) => v,
                        Err(error) => return Err(error),
                    };
                    Ok((v6, res))
                });

                let mut res = Vec::new();

                match v4_handle.await {
                    Ok(Ok((udp, results))) => {
                        res.extend(results.into_iter());
                        self.udp4 = Some(udp);
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(error) => {
                        tracing::error!(?error, "failed to receive from v4");
                    }
                }

                match v6_handle.await {
                    Ok(Ok((udp, results))) => {
                        res.extend(results.into_iter());
                        self.udp6 = Some(udp);
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(error) => {
                        tracing::error!(?error, "failed to receive from v6");
                    }
                }

                res
            }
            (_, Some(mut udp)) | (Some(mut udp), _) => {
                let results = listen_for_responses(&mut udp, 4_000).await?;
                if has_v4 {
                    self.udp4 = Some(udp);
                } else {
                    self.udp6 = Some(udp);
                }

                results
            }
            _ => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                vec![]
            }
        };

        for (remote, status) in results {
            tracing::info!(?remote, ?status, "got pong");
            self.latest_pings.insert(remote, status);
        }

        Ok(())
    }

    async fn send_pings(&mut self) -> std::io::Result<u64> {
        let config = {
            let config = self.config.agent_config.read().await;
            Arc::clone(&*config)
        };

        let mut ping_targets = vec![];
        for hostname in &config.ping_target_addresses {
            let targets = address_lookup(hostname, 5530).await;
            for address in targets {
                ping_targets.push(PingTarget {
                    address,
                    hostname: hostname.clone(),
                });
            }
        }

        if ping_targets.len() == 0 {
            return Ok(0);
        }

        let mut count = 0;

        for target in &ping_targets {
            let (udp, current_ping) = match self.get_socket_and_ping(&target.address.ip()).await {
                Ok(v) => v,
                Err(error) => {
                    tracing::error!(?error, "failed to setup UDP socket");
                    continue;
                }
            };

            count += 1;

            let mut send_buf = vec![0u8; 256];
            let mut writer = &mut send_buf[..];

            /* ping version 2 */
            writer.write_u32::<BigEndian>(2).unwrap();

            /* write current ping */
            writer.write_u64::<BigEndian>(current_ping).unwrap();

            /* write current timestamp */
            let now = now_milli();
            writer.write_u64::<BigEndian>(now).unwrap();

            if target.hostname.len() < 30 {
                writer.write_u32::<BigEndian>(target.hostname.len() as u32).unwrap();
                writer.write(target.hostname.as_bytes()).unwrap();
            }
            else {
                writer.write_u32::<BigEndian>(0).unwrap();
            }

            let data = {
                let no_written = writer.len();
                &send_buf[..send_buf.len() - no_written]
            };

            udp.send_to(data, target.address).await?;
        }

        Ok(count)
    }

    async fn get_socket_and_ping(&mut self, ip: &IpAddr) -> std::io::Result<(&UdpSocket, u64)> {
        let udp = match ip {
            IpAddr::V4(_) => {
                if self.udp4.is_none() {
                    let udp = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await?;
                    self.udp4 = Some(udp);
                }

                self.udp4.as_ref().unwrap()
            }
            IpAddr::V6(_) => {
                if self.udp6.is_none() {
                    let udp = UdpSocket::bind(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)).await?;
                    self.udp6 = Some(udp);
                }

                self.udp6.as_ref().unwrap()
            }
        };

        Ok((
            udp,
            self.latest_pings.get(ip).map(|v| v.ping).unwrap_or(0)
        ))
    }
}

async fn listen_for_responses(udp: &mut UdpSocket, time_milli: u64) -> std::io::Result<Vec<(IpAddr, PingStatus)>> {
    let end = Instant::now().add(Duration::from_millis(time_milli));
    let mut results = Vec::new();
    let mut buf = [0u8; 40];

    while Instant::now() < end {
        let (bytes, remote) = match tokio::time::timeout_at(end, udp.recv_from(&mut buf)).await {
            Ok(Ok(v)) => v,
            Ok(Err(error)) => return Err(error),
            Err(_) => break,
        };

        let received = &buf[..bytes];
        if received.len() != 20 {
            tracing::error!(size = received.len(), "got pong with invalid size");
            continue;
        }

        let version = BigEndian::read_u32(&received[0..4]);
        if version != 1 {
            tracing::error!(version, "got unsupported pong version");
            continue;
        }

        let tunnel_server_id = BigEndian::read_u64(&received[4..12]);
        let sent_epoch_milli = BigEndian::read_u64(&received[12..20]);

        let now = now_milli();
        if now < sent_epoch_milli {
            tracing::error!(version, "invalid timestamp in pong");
            continue;
        }

        let ping = now - sent_epoch_milli;
        results.push((remote.ip(), PingStatus {
            tunnel_server_id,
            ping,
        }));
    }

    Ok(results)
}

struct PingTarget {
    address: SocketAddr,
    hostname: String,
}