//! Built-in NetherNet host for Bedrock Dedicated Server.
//!
//! A NetherNet tunnel's players speak WebRTC, which Bedrock Dedicated Server only accepts
//! through a proxy such as WaterdogPE. Those proxies re-sign each player's login, so the server
//! no longer sees the player's Xbox identity and treats returning players as new ones.
//!
//! The bridge replaces the proxy. It registers with playit's NetherNet signaling service using
//! the tunnel's token, terminates each player's WebRTC connection, and relays their game
//! packets to the server over RakNet without decoding them. The server keeps `online-mode=true`
//! and authenticates players itself, exactly as it did on a plain Bedrock tunnel.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use str0m::config::DtlsCert;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use self::admission::{AdmissionKeys, UsedTokens};
use self::nxs::{HostProfile, NxsHost, NxsToken, ServerSample};
use crate::network::origin_lookup::OriginResource;
use crate::utils::now_milli;

pub mod admission;
pub mod nxs;
pub mod raknet;
pub mod session;
pub mod stun;

/// For testing before run data carries tokens: `PLAYIT_NETHERNET_TOKENS=<tunnel id>=<token>,...`.
const TEST_TOKENS_ENV: &str = "PLAYIT_NETHERNET_TOKENS";

const MAX_SESSIONS: usize = 512;
const STATUS_TIMEOUT: Duration = Duration::from_secs(2);
const SCTP_PORT: u16 = 5000;
const MAX_MESSAGE_SIZE: u32 = 262_144;
/* Used when the server hides its status (enable-lan-visibility=false). */
const DEFAULT_SERVER_NAME: &str = "Bedrock Dedicated Server";
const DEFAULT_MAX_PLAYERS: u64 = 100;

/// Looks up the NetherNet token for a tunnel: from run data, or the test override.
pub fn token_for(tunnel_id: u64, from_run_data: Option<&str>) -> Option<NxsToken> {
    if let Some(token) = from_run_data.and_then(NxsToken::parse) {
        return Some(token);
    }
    let overrides = std::env::var(TEST_TOKENS_ENV).ok()?;
    overrides.split(',').find_map(|entry| {
        let (id, token) = entry.split_once('=')?;
        (id.trim().parse::<u64>().ok()? == tunnel_id)
            .then(|| NxsToken::parse(token))
            .flatten()
    })
}

/// The bridges this agent runs, one per NetherNet tunnel with a token.
#[derive(Default)]
pub struct NetherNetBridges {
    state_dir: Option<PathBuf>,
    running: HashMap<u64, RunningBridge>,
}

struct RunningBridge {
    token: NxsToken,
    local_addr: SocketAddr,
    origin: watch::Sender<OriginResource>,
    cancel: CancellationToken,
}

impl Drop for RunningBridge {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl NetherNetBridges {
    pub fn set_state_dir(&mut self, dir: PathBuf) {
        self.state_dir = Some(dir);
    }

    /// Starts, updates, or stops bridges so exactly `wanted` run. Each entry is the tunnel's
    /// own origin (the Bedrock server) and its token. Returns each bridge's local address,
    /// where the tunnel's UDP should go instead.
    pub async fn update(&mut self, wanted: Vec<(OriginResource, NxsToken)>) -> HashMap<u64, SocketAddr> {
        let mut keep = HashMap::new();
        for (origin, token) in wanted {
            let tunnel_id = origin.tunnel_id;
            match self.running.remove(&tunnel_id) {
                Some(running) if running.token == token && !running.cancel.is_cancelled() => {
                    let _ = running.origin.send(origin);
                    keep.insert(tunnel_id, running);
                }
                _ => match self.start(origin, token).await {
                    Ok(running) => {
                        keep.insert(tunnel_id, running);
                    }
                    Err(error) => {
                        tracing::error!(?error, tunnel_id, "failed to start NetherNet bridge");
                    }
                },
            }
        }
        /* Bridges left in `self.running` are no longer wanted; dropping them stops them. */
        self.running = keep;
        self.running.iter().map(|(id, running)| (*id, running.local_addr)).collect()
    }

    async fn start(&self, origin: OriginResource, token: NxsToken) -> std::io::Result<RunningBridge> {
        let socket = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
        let local_addr = socket.local_addr()?;
        let cert = str0m::crypto::from_feature_flags()
            .dtls_provider
            .generate_certificate()
            .ok_or_else(|| std::io::Error::other("could not generate DTLS certificate"))?;

        let mut incarnation = [0u8; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut incarnation)
            .map_err(|_| std::io::Error::other("random"))?;
        let incarnation = hex::encode(incarnation);

        let profile = HostProfile {
            dtls_fingerprint: session::fingerprint_string(&cert),
            incarnation: incarnation.clone(),
            candidate: local_addr,
            sctp_port: SCTP_PORT,
            max_message_size: MAX_MESSAGE_SIZE,
        };

        let tunnel_id = origin.tunnel_id;
        let (origin_tx, origin_rx) = watch::channel(origin);
        let (keys_tx, keys_rx) = watch::channel(Arc::new(AdmissionKeys::new(&incarnation, Vec::new())));
        let cancel = CancellationToken::new();

        let state_path = self
            .state_dir
            .as_ref()
            .map(|dir| dir.join(format!("nethernet-{tunnel_id}.json")));
        if state_path.is_none() {
            tracing::warn!(tunnel_id, "no state directory for NetherNet registration; it will not survive a restart");
        }

        let host = NxsHost::new(token.clone(), state_path, profile, keys_tx);
        let status_origin = origin_rx.clone();
        tokio::spawn(host.run(cancel.child_token(), move || {
            let origin = status_origin.borrow().clone();
            sample_server(origin)
        }));

        tokio::spawn(
            Listener {
                socket: Arc::new(socket),
                local_addr,
                cert,
                keys: keys_rx,
                origin: origin_rx,
                sessions: HashMap::new(),
                used_tokens: UsedTokens::default(),
            }
            .run(cancel.child_token()),
        );

        tracing::info!(tunnel_id, %local_addr, "started NetherNet bridge");
        Ok(RunningBridge {
            token,
            local_addr,
            origin: origin_tx,
            cancel,
        })
    }
}

async fn sample_server(origin: OriginResource) -> ServerSample {
    let sampled_at_ms = now_milli();
    let Some(server) = origin.resolve_local(0).await else {
        return ServerSample::default();
    };
    match raknet::query_status(server, STATUS_TIMEOUT).await {
        Some(status) => ServerSample {
            reachable: true,
            name: if status.motd.trim().is_empty() { DEFAULT_SERVER_NAME.to_string() } else { status.motd },
            level: status.level,
            players: status.players,
            max_players: if status.max_players == 0 { DEFAULT_MAX_PLAYERS } else { status.max_players },
            game_type: status.game_type,
            protocol: status.protocol,
            version: status.version,
            sampled_at_ms,
        },
        None => {
            tracing::warn!(%server, "Bedrock server did not answer a status ping; NetherNet players are paused until it does");
            ServerSample {
                sampled_at_ms,
                ..ServerSample::default()
            }
        }
    }
}

struct Listener {
    socket: Arc<UdpSocket>,
    local_addr: SocketAddr,
    cert: DtlsCert,
    keys: watch::Receiver<Arc<AdmissionKeys>>,
    origin: watch::Receiver<OriginResource>,
    sessions: HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>,
    used_tokens: UsedTokens,
}

impl Listener {
    async fn run(mut self, cancel: CancellationToken) {
        let (ended_tx, mut ended_rx) = mpsc::unbounded_channel::<SocketAddr>();
        let mut buf = vec![0u8; 2048];

        loop {
            let (len, source) = tokio::select! {
                _ = cancel.cancelled() => return,
                Some(source) = ended_rx.recv() => {
                    self.sessions.remove(&source);
                    continue;
                }
                received = self.socket.recv_from(&mut buf) => match received {
                    Ok(received) => received,
                    /* Windows reports a closed peer port as a receive error. */
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => continue,
                    Err(error) => {
                        tracing::error!(?error, "NetherNet bridge socket failed");
                        return;
                    }
                },
            };
            let packet = &buf[..len];

            if let Some(session) = self.sessions.get(&source) {
                if session.try_send(packet.to_vec()).is_err() && session.is_closed() {
                    self.sessions.remove(&source);
                }
                continue;
            }

            if MAX_SESSIONS <= self.sessions.len() {
                continue;
            }
            let now = now_milli();
            let keys = self.keys.borrow().clone();
            let admitted = match keys.admit(packet, now) {
                Ok(admitted) => admitted,
                Err(error) => {
                    tracing::debug!(?error, %source, "refused NetherNet packet from unknown source");
                    continue;
                }
            };
            if !self.used_tokens.claim(&admitted, source, now) {
                continue;
            }

            let origin = self.origin.borrow().clone();
            let Some(server) = origin.resolve_local(0).await else {
                tracing::warn!(tunnel_id = origin.tunnel_id, "could not resolve the Bedrock server address for a NetherNet player");
                continue;
            };

            let (tx, rx) = mpsc::channel(1024);
            self.sessions.insert(source, tx);

            let setup = session::SessionSetup {
                admitted,
                source,
                local: self.local_addr,
                socket: self.socket.clone(),
                cert: self.cert.clone(),
                server,
                first_packet: packet.to_vec(),
                from_client: rx,
            };
            let ended_tx = ended_tx.clone();
            let cancel = cancel.child_token();
            tokio::spawn(async move {
                tracing::info!(%source, %server, "NetherNet player connecting");
                let end = tokio::select! {
                    end = session::run(setup) => Some(end),
                    _ = cancel.cancelled() => None,
                };
                if let Some(end) = end {
                    tracing::info!(%source, ?end, "NetherNet player disconnected");
                }
                let _ = ended_tx.send(source);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use str0m::channel::{ChannelConfig, Reliability};
    use str0m::crypto::Fingerprint;
    use str0m::net::{Protocol, Receive};
    use str0m::{Candidate, Event, IceCreds, Input, Output, RtcConfig};

    use super::admission::tests::{INCARNATION, SECRET, fixture_keys, mint};
    use super::admission::{audience, host_ice_password};
    use super::*;
    use crate::network::origin_lookup::{OriginIp, OriginTarget};

    /// Plays a Bedrock client against the bridge: ICE and DTLS through an admission token,
    /// then a RequestNetworkSettings batch that only a real server answers.
    ///
    /// Needs a Bedrock Dedicated Server with `transport=raknet`:
    /// `NETHERNET_TEST_BDS=127.0.0.1:19232 cargo test -p playit-agent-core -- --ignored bridge_reaches`
    #[tokio::test]
    #[ignore]
    async fn bridge_reaches_bedrock_server() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
        let server: SocketAddr = std::env::var("NETHERNET_TEST_BDS")
            .unwrap_or_else(|_| "127.0.0.1:19232".into())
            .parse()
            .unwrap();
        let protocol = raknet::query_status(server, Duration::from_secs(2))
            .await
            .expect("server answers status ping")
            .protocol
            .unwrap_or(2225);

        /* Bridge side. */
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bridge_addr = socket.local_addr().unwrap();
        let provider = str0m::crypto::from_feature_flags();
        let bridge_cert = provider.dtls_provider.generate_certificate().unwrap();
        let bridge_fingerprint = ring::digest::digest(&ring::digest::SHA256, &bridge_cert.certificate);
        let (_keys_tx, keys_rx) = watch::channel(Arc::new(fixture_keys()));
        let origin = OriginResource {
            tunnel_id: 1,
            proto: playit_agent_proto::PortProto::Udp,
            target: OriginTarget::Port {
                ip: OriginIp::IpAddress(server.ip()),
                port: server.port(),
            },
            port_count: 0,
            proxy_protocol: None,
        };
        let (_origin_tx, origin_rx) = watch::channel(origin);
        let cancel = CancellationToken::new();
        tokio::spawn(
            Listener {
                socket: Arc::new(socket),
                local_addr: bridge_addr,
                cert: bridge_cert,
                keys: keys_rx,
                origin: origin_rx,
                sessions: HashMap::new(),
                used_tokens: UsedTokens::default(),
            }
            .run(cancel.clone()),
        );

        /* Client side: what the signaling service would have handed a player. */
        let client_cert = provider.dtls_provider.generate_certificate().unwrap();
        let client_fingerprint: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, &client_cert.certificate)
            .as_ref()
            .try_into()
            .unwrap();
        let client_ufrag = "clientUfragTest";
        let client_password = "clientPassword0123456789";
        let keys = fixture_keys();
        let token = mint(&keys, client_ufrag, client_password, client_fingerprint, (now_milli() / 1000) as u32 + 60);
        let host_password = host_ice_password(SECRET.as_bytes(), &audience(INCARNATION), &token);

        let client_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client_socket.local_addr().unwrap();
        let mut rtc = RtcConfig::new()
            .set_local_ice_credentials(IceCreds {
                ufrag: client_ufrag.into(),
                pass: client_password.into(),
            })
            .set_dtls_cert(client_cert)
            .build(Instant::now());
        rtc.add_local_candidate(Candidate::host(client_addr, "udp").unwrap());
        rtc.add_remote_candidate(Candidate::host(bridge_addr, "udp").unwrap());
        let channel_id = {
            let mut api = rtc.direct_api();
            /* The client's str0m has the same username limit; give it a stand-in too. */
            api.set_remote_ice_credentials(IceCreds {
                ufrag: "hostStandIn".into(),
                pass: host_password.clone(),
            });
            api.set_ice_controlling(true);
            api.set_remote_fingerprint(Fingerprint {
                hash_func: "sha-256".into(),
                bytes: bridge_fingerprint.as_ref().to_vec(),
            });
            api.start_dtls(false).unwrap();
            /* libwebrtc, inside the Bedrock client, sends its own SCTP INIT as well. */
            api.start_sctp(true);
            api.create_data_channel(ChannelConfig {
                label: session::RELIABLE_CHANNEL.into(),
                ordered: true,
                reliability: Reliability::Reliable,
                negotiated: None,
                protocol: String::new(),
            })
        };

        /* RequestNetworkSettings (id 193) as an uncompressed batch: length, id, protocol. */
        let mut request = vec![0x06, 0xc1, 0x01];
        request.extend_from_slice(&(protocol as i32).to_be_bytes());

        let mut sent = false;
        let mut assembler = session::SegmentAssembler::default();
        let mut buf = vec![0u8; 2048];
        let deadline = Instant::now() + Duration::from_secs(15);
        let response = 'client: loop {
            assert!(Instant::now() < deadline, "no NetworkSettings from the server");
            let timeout = loop {
                match rtc.poll_output().unwrap() {
                    Output::Timeout(at) => break at,
                    Output::Transmit(transmit) => {
                        let rewritten = stun::rewrite_username(
                            &transmit.contents,
                            &format!("hostStandIn:{client_ufrag}"),
                            &format!("{token}:{client_ufrag}"),
                            &host_password,
                        );
                        let contents = rewritten.as_deref().unwrap_or(&transmit.contents);
                        client_socket.send_to(contents, transmit.destination).await.unwrap();
                    }
                    Output::Event(Event::ChannelOpen(id, _)) if id == channel_id && !sent => {
                        let segment = session::segment(&request).unwrap().remove(0);
                        assert!(rtc.channel(id).unwrap().write(true, &segment).unwrap());
                        sent = true;
                    }
                    Output::Event(Event::ChannelData(data)) => {
                        if let Some(message) = assembler.push(&data.data).unwrap() {
                            break 'client message;
                        }
                    }
                    Output::Event(_) => {}
                }
            };
            let wait = timeout.saturating_duration_since(Instant::now()).min(Duration::from_millis(50));
            match tokio::time::timeout(wait, client_socket.recv_from(&mut buf)).await {
                Ok(Ok((len, source))) => {
                    let rewritten = stun::rewrite_username(
                        &buf[..len],
                        &format!("{client_ufrag}:{token}"),
                        &format!("{client_ufrag}:hostStandIn"),
                        client_password,
                    );
                    let packet = rewritten.as_deref().unwrap_or(&buf[..len]);
                    let receive = Receive::new(Protocol::Udp, source, client_addr, packet).unwrap();
                    rtc.handle_input(Input::Receive(Instant::now(), receive)).unwrap();
                }
                _ => rtc.handle_input(Input::Timeout(Instant::now())).unwrap(),
            }
        };

        /* NetworkSettings is packet id 143. */
        assert_eq!(&response[1..3], &[0x8f, 0x01], "unexpected reply {response:02x?}");
        cancel.cancel();
    }
}
