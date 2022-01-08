use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use byteorder::{BigEndian, ByteOrder};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::channel;
use tokio::sync::RwLock;
use tracing::Level;
use agent::api_client::ApiClient;
use agent::config::load_or_create;
use agent::now_milli;

use messages::{ClaimInstructions, ClaimLease, ClaimProto, Proto, SetupUdpChannelDetails};

use agent::tunnel_client::TunnelClient;
use agent::tcp_client::{Stats, TcpConnection};
use agent::udp_client::UdpClients;
use messages::udp::{UDP_CHANNEL_ESTABLISH_ID, REDIRECT_FLOW_FOOTER_ID, RedirectFlowFooter};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_max_level(Level::INFO).with_writer(std::io::stderr).init();
    let config = Arc::new(load_or_create().await.unwrap().unwrap());
    let tunnel_udp = Arc::new(UdpSocket::bind(SocketAddrV4::new(0.into(), 0)).await.unwrap());

    let mut lease_claims = Vec::new();
    for mapping in &config.mappings {
        lease_claims.push(ClaimLease {
            ip: mapping.tunnel_ip,
            from_port: mapping.tunnel_from_port,
            to_port: mapping.tunnel_to_port.unwrap_or(mapping.tunnel_from_port + 1),
            proto: mapping.proto,
        });
    }

    let api_url = match &config.api_url {
        Some(v) => v.clone(),
        None => "https://api.playit.cloud/agent".to_string(),
    };

    let (tx, mut rx) = channel(1024);
    let api_client = ApiClient::new(api_url, Some(config.secret_key.clone()));
    let tunnel_client = TunnelClient::new(api_client, tx).await.unwrap();

    let udp_channel_details = Arc::new(RwLock::new(None)); // RwLock<Option<SetupUdpChannelDetails>>
    let last_udp_time = Arc::new(AtomicU64::new(now_milli()));
    let mut udp_clients = UdpClients::new(tunnel_udp.clone(), udp_channel_details.clone());

    let keep_alive_task = {
        let client = tunnel_client.clone();
        let udp_channel_details = udp_channel_details.clone();
        let tunnel_udp = tunnel_udp.clone();
        let last_udp_time = last_udp_time.clone();

        tokio::spawn(async move {
            'keep_alive: loop {
                tokio::time::sleep(Duration::from_secs(2)).await;

                let authenticated = client.keep_alive().await;

                if !authenticated.unwrap_or(false) {
                    let register_res = client.register().await;
                    if let Err(error) = register_res {
                        tracing::error!(?error, "failed to register agent");
                        continue;
                    }

                    for claim in &lease_claims {
                        let claim_res = client.claim_lease(claim.clone()).await;

                        if let Err(error) = claim_res {
                            tracing::error!(?error, ?claim, "failed to claim lease");
                            continue 'keep_alive;
                        }
                    }

                    tracing::info!("Connection to tunnel (re)established");
                }

                if now_milli() - last_udp_time.load(Ordering::SeqCst) > 60_000 {
                    let mut lock = udp_channel_details.write().await;
                    if lock.is_some() {
                        tracing::warn!("UDP silence detected trying to setup again");
                        let _ = lock.take();
                    }
                }

                /* setup udp channel */
                {
                    let needs_setup = {
                        udp_channel_details.read().await.is_none()
                    };

                    if needs_setup {
                        let res = match client.setup_udp_channel().await {
                            Ok(v) => v,
                            Err(error) => {
                                tracing::error!(?error, "failed to setup udp channel");
                                continue;
                            }
                        };

                        let mut lock = udp_channel_details.write().await;
                        lock.replace(res);
                    }
                }

                /* keep udp channel alive */
                {
                    let lock = udp_channel_details.read().await;
                    if let Some(channel) = lock.as_ref() {
                        if let Err(error) = tunnel_udp.send_to(&channel.token, channel.tunnel_addr).await {
                            tracing::error!(?error, "failed to send message to UDP channel");
                        }

                        continue;
                    }
                }
            }
        })
    };

    let udp_channel_task = {
        let config = config.clone();

        tokio::spawn(async move {
            let mut buffer = vec![0u8; 2048];
            // let mut last_message_time = 0; // TODO

            loop {
                let (bytes, from) = match tunnel_udp.recv_from(&mut buffer).await {
                    Ok(v) => v,
                    Err(error) => {
                        tracing::error!(?error, "failed reading from UDP channel");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                if bytes < 8 {
                    tracing::warn!(bytes, "got invalid tiny UDP channel message");
                    continue;
                }

                let id = BigEndian::read_u64(&buffer[bytes - 8..bytes]);
                if id == UDP_CHANNEL_ESTABLISH_ID {
                    last_udp_time.store(now_milli(), Ordering::SeqCst);

                    tracing::info!("got UDP establish response");
                    continue;
                }

                let flow = match RedirectFlowFooter::from_tail(&buffer[..bytes]) {
                    Some(v) => v,
                    None => {
                        tracing::error!(id, bytes, ?from, "got channel message with unknown id");
                        continue;
                    }
                };

                let payload = &buffer[..bytes - RedirectFlowFooter::len()];

                udp_clients.forward_packet(flow, payload, |addr| {
                    config.find_local_addr(addr, Proto::Udp)
                }).await;
            }
        })
    };

    while let Some(client) = rx.recv().await {
        println!("Got client: {:?}", client);

        match client.claim_instructions {
            ClaimInstructions::Tcp { address, token } => {
                println!("Token length: {}", token.len());

                let (_, host_addr) = match config.find_local_addr(client.connect_addr, Proto::Tcp) {
                    Some(host_addr) => {
                        tracing::info!(?host_addr, "found local address for new tcp client");
                        host_addr
                    }
                    None => {
                        tracing::error!(?client.connect_addr, "did not find local address for new tcp client");
                        continue
                    },
                };

                let tcp_conn = TcpConnection {
                    client_token: token,
                    tunnel_address: address,
                };

                let ready = match tcp_conn.establish().await {
                    Ok(v) => v,
                    Err(error) => {
                        tracing::error!(?error, "failed to establish connection to tunnel server");
                        continue;
                    }
                };

                let active = match ready.connect_to_host(host_addr, Arc::new(Stats::default())).await {
                    Ok(v) => v,
                    Err(error) => {
                        tracing::error!(?error, "failed to connect to local service");
                        continue;
                    }
                };

                tracing::info!(stats = ?active.stats, "connection setup");
            }
        }
    }

    keep_alive_task.await.unwrap();
    udp_channel_task.await.unwrap();
}

pub async fn pipe(mut from: OwnedReadHalf, mut to: OwnedWriteHalf) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    buffer.resize(2048, 0u8);

    loop {
        tokio::task::yield_now().await;

        let received = from.read(&mut buffer[..]).await.map_err(|error| {
            tracing::error!(?error, "failed to read data");
            error
        })?;

        if received == 0 {
            tracing::info!("pipe ended due to EOF");
            break;
        }

        to.write_all(&buffer[..received]).await.map_err(|error| {
            tracing::error!(?error, "failed to write data");
            error
        })?;
    }

    Ok(())
}