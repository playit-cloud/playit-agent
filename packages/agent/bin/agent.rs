use std::net::SocketAddrV4;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use byteorder::{BigEndian, ByteOrder};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::channel;
use tokio::sync::RwLock;
use tracing::Level;

use agent::api_client::{ApiClient, ApiError};
use agent::agent_config::load_or_create;
use agent::now_milli;
use agent::tcp_client::{Stats, TcpConnection};
use agent::tunnel_client::TunnelClient;
use agent::udp_client::UdpClients;
use messages::{ClaimInstructions, ClaimLease, Ping, Proto, TunnelRequest};
use messages::agent_config::{AgentConfig, DEFAULT_API};
use messages::udp::{RedirectFlowFooter, UDP_CHANNEL_ESTABLISH_ID};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    let config = Arc::new(RwLock::new(prepare_config().await));

    if config.read().await.refresh_from_api {
        tokio::spawn(update_agent_config(config.clone()));
    }

    let tunnel_udp = Arc::new(
        UdpSocket::bind(SocketAddrV4::new(0.into(), 0))
            .await
            .unwrap(),
    );

    let mut lease_claims = Vec::new();
    for mapping in &config.read().await.mappings {
        lease_claims.push(ClaimLease {
            ip: mapping.tunnel_ip,
            from_port: mapping.tunnel_from_port,
            to_port: mapping
                .tunnel_to_port
                .unwrap_or(mapping.tunnel_from_port + 1),
            proto: mapping.proto,
        });
    }

    let api_url = config.read().await.get_api_url();

    let (tx, mut rx) = channel(1024);
    let api_client = ApiClient::new(api_url, Some(config.read().await.secret_key.clone()));
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
                    let needs_setup = { udp_channel_details.read().await.is_none() };

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
                        if let Err(error) = tunnel_udp
                            .send_to(&channel.token, channel.tunnel_addr)
                            .await
                        {
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

                let config_read = config.read().await;

                udp_clients
                    .forward_packet(flow, payload, |addr| {
                        config_read.find_local_addr(addr, Proto::Udp)
                    })
                    .await;
            }
        })
    };

    while let Some(client) = rx.recv().await {
        println!("Got client: {:?}", client);

        match client.claim_instructions {
            ClaimInstructions::Tcp { address, token } => {
                println!("Token length: {}", token.len());

                let (_, host_addr) = match config.read().await.find_local_addr(client.connect_addr, Proto::Tcp) {
                    Some(host_addr) => {
                        tracing::info!(?host_addr, "found local address for new tcp client");
                        host_addr
                    }
                    None => {
                        tracing::error!(?client.connect_addr, "did not find local address for new tcp client");
                        continue;
                    }
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

                let active = match ready
                    .connect_to_host(host_addr, Arc::new(Stats::default()))
                    .await
                {
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

async fn update_agent_config(config: Arc<RwLock<AgentConfig>>) {
    loop {
        let api = {
            let c = config.read().await;
            ApiClient::new(c.get_api_url(), Some(c.secret_key.clone()))
        };

        let mut api_config = match api.get_agent_config().await {
            Ok(config) => config,
            Err(error) => {
                tracing::error!(?error, "failed to load config from API");
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        };

        let config_updated = {
            let current = config.read().await;

            if let Some(ref api_url) = current.api_url {
                api_config.api_url = Some(api_url.clone());
            }

            !api_config.eq(&current)
        };

        if config_updated {
            tracing::info!("updating config");
            std::mem::replace(&mut *config.write().await, api_config.clone());

            if let Err(error) = tokio::fs::write("playit.toml", toml::to_string_pretty(&api_config).unwrap()).await {
                tracing::error!(?error, "failed to write updated configuration to playit.toml");
            }
        }

        tokio::time::sleep(Duration::from_secs(4)).await;
    }
}

async fn prepare_config() -> AgentConfig {
    let config = match load_or_create().await {
        Ok(Some(config)) => {
            if config.valid_secret_key() {
                let api = ApiClient::new(config.get_api_url(), Some(config.secret_key.clone()));

                /* see if we're allowed to sign a request */
                let error = api.sign_tunnel_request(TunnelRequest::Ping(Ping {
                    id: 0
                })).await.err();

                match error {
                    Some(ApiError::HttpError(401, _)) => {
                        tracing::warn!("failed to validate secret key");
                    }
                    Some(error) => {
                        tracing::error!(?error, "got error trying to validate secret key");
                        std::process::exit(1);
                    }
                    None => {
                        return config;
                    }
                }
            }

            Some(config)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::error!(?error, "failed to load / create config file");
            std::process::exit(1);
        }
    };

    tracing::info!("generating claim key to setup playit program");

    let mut buffer = [0u8; 32];
    SystemRandom::new().fill(&mut buffer).unwrap();
    let claim_key = hex::encode(&buffer);

    let claim_url = format!("https://new.playit.gg/claim/{}", claim_key);
    if let Err(error) = webbrowser::open(&claim_url) {
        tracing::error!(?error, "failed to open claim URL in web browser");
        println!("\n******************\n\nOpen below link a web browser to continue\n{}\n\n******************", claim_url);
    }

    let api_url = config.as_ref().map(|v| v.get_api_url()).unwrap_or(DEFAULT_API.to_string());
    let api = ApiClient::new(api_url, None);

    /*
     * Keep polling api till secret key has been generated. For the secret
     * to be generated the user must interact with the website using the
     * claim URL.
     */
    let secret_key = loop {
        match api.try_exchange_claim_for_secret(&claim_key).await {
            Ok(Some(secret_key)) => break secret_key,
            Ok(None) => {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => {
                tracing::error!(?error, "failed to exchange claim key for secret key");
                tokio::time::sleep(Duration::from_secs(8)).await;
            }
        }
    };

    tracing::info!("agent setup, got secret key");

    let config = match config {
        Some(mut config) => {
            config.secret_key = secret_key;
            config
        }
        None => {
            AgentConfig {
                api_url: None,
                refresh_from_api: true,
                secret_key,
                mappings: vec![],
            }
        }
    };

    tokio::fs::write("playit.toml", toml::to_string_pretty(&config).unwrap()).await.unwrap();
    tracing::info!("playit.toml updated");

    config
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
