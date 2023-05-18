
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::Level;

use playit_agent_core::tunnel::setup::SetupFindSuitableChannel;
use playit_agent_core::tunnel::tcp_tunnel::TcpTunnel;
use playit_agent_core::tunnel::udp_tunnel::{UdpTunnel, UdpTunnelRx};
use playit_agent_core::utils::error_helper::ErrorHelper;
use playit_agent_core::utils::name_lookup::address_lookup;
use playit_agent_core::utils::now_milli;
use playit_agent_proto::control_feed::ControlFeed;
use playit_agent_proto::control_messages::{ControlResponse};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_ansi(false).with_max_level(Level::INFO).init();

    let addresses = address_lookup("control.playit.gg", 5525).await;
    let setup = SetupFindSuitableChannel::new(addresses).setup().await.unwrap();

    let mut control_channel = setup.authenticate("<replace me>".to_string()).await.unwrap();
    tracing::info!("agent setup");

    let mut last_keep_alive = 0;
    let mut last_ping = 0;

    let udp_tunnel = UdpTunnel::new().await.unwrap();

    {
        let udp_tunnel = udp_tunnel.clone();
        let mut buffer = vec![0u8; 2048];
        let mut last_confirm = 0;

        tokio::spawn(async move {
            loop {
                buffer.resize(2048, 0);

                match tokio::time::timeout(Duration::from_secs(2), udp_tunnel.receive_from(&mut buffer)).await {
                    Err(_) => {
                        if udp_tunnel.requires_resend() {
                            udp_tunnel.resend_token().await.take_error(|error| {
                                tracing::error!(?error, "failed to resend setup");
                            });
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::error!(?error, "got error reading from socket");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    Ok(Ok(UdpTunnelRx::ConfirmedConnection)) => {
                        last_confirm = now_milli();
                    }
                    Ok(Ok(UdpTunnelRx::ReceivedPacket { bytes, flow })) => {
                        tracing::info!("got {} bytes with {:?}", bytes, flow);
                        buffer.truncate(bytes);

                        udp_tunnel.send(&mut buffer, flow.flip()).await.take_error(|error| {
                            tracing::error!(?error, "failed to send reply to client");
                        });
                    }
                }
            }
        });
    }

    loop {
        match tokio::time::timeout(Duration::from_secs(10), control_channel.recv_feed_msg()).await {
            Ok(Ok(ControlFeed::NewClient(new_client))) => {
                tracing::info!(?new_client, "got new client");

                tokio::spawn(async move {
                    let mut tcp = TcpTunnel::new(new_client.claim_instructions.clone(), new_client.peer_addr).connect().await
                        .unwrap();

                    let mut buffer = vec![0u8; 2048];
                    while let Ok(bytes) = tcp.read(&mut buffer).await {
                        if bytes == 0 {
                            break;
                        }
                        if let Err(error) = tcp.write_all(&buffer[..bytes]).await {
                            tracing::error!(?error, "failed to write");
                            break;
                        }
                    }
                });
            }
            Ok(Ok(ControlFeed::Response(msg))) => {
                match msg.content {
                    ControlResponse::UdpChannelDetails(details) => {
                        tracing::info!("setup udp tunnel");
                        udp_tunnel.set_udp_tunnel(details).await.unwrap();
                    }
                    msg => {
                        tracing::info!(?msg, "got response");
                    }
                }
            }
            Ok(Err(error)) => {
                tracing::error!(?error, "failed to parse response");
            }
            Err(_) => {}
        }

        let now = now_milli();

        if now - last_ping > 5_000 {
            last_ping = now;
            if let Err(error) = control_channel.send_ping(200, now).await {
                tracing::error!(?error, "failed to send ping");
            }

            if !udp_tunnel.is_setup().await {
                control_channel.send_setup_udp_channel(1).await.take_error(|error| {
                    tracing::error!(?error, "failed to send setup udp channel request");
                });
            }
        }

        let time_till_expire = control_channel.get_expire_at().max(now) - now;
        tracing::info!(time_till_expire, "time till expire");

        /* 30 seconds till expiry and haven't sent in last 10 sec */
        if 10_000 < now - last_keep_alive && time_till_expire < 30_000 {
            last_keep_alive = now;

            tracing::info!("sent KeepAlive");
            if let Err(error) = control_channel.send_keep_alive(100).await {
                tracing::error!(?error, "failed to send KeepAlive");
            }
        }
    }
}
