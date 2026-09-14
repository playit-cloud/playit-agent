//! UDP tunnels.
//!
//! [`UdpTunnel`] owns the datagram channel to the tunnel server and the table of
//! virtual clients that relay to local origins. It runs as one task; per-client reply
//! readers live in [`flows`] and feed back through a channel.
//!
//! Wire format: every datagram between agent and tunnel server carries a
//! [`UdpFlow`] footer identifying the remote client and the tunnel. The channel is
//! authenticated by sending the session token from the control channel and confirmed by
//! an 8-byte `UDP_CHANNEL_ESTABLISH_ID` datagram from the server.

pub mod flows;
pub mod packet;

use std::{net::SocketAddr, num::NonZeroU32, sync::Arc, time::Duration};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use playit_agent_proto::{
    control_messages::UdpChannelDetails,
    udp_proto::{UDP_CHANNEL_ESTABLISH_ID, UdpFlow},
};
use playit_api_client::api::ProxyProtocol;
use tokio::{
    sync::mpsc,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

use crate::{
    control::{ControlHandle, DualStackUdpSocket},
    origin::{OriginLookup, Transport, lan, proxy_protocol::ProxyProtocolHeader},
    stats::AgentStats,
};
use flows::{ClientKey, FlowTable, OriginPacket, UdpFlowDetails};
use packet::{Packet, PacketPool};

#[derive(Debug, Clone)]
pub struct UdpSettings {
    /// New virtual clients accepted per second and the burst allowed on top.
    pub new_client_ratelimit: u32,
    pub new_client_ratelimit_burst: u32,
    /// Buffers per direction; bounds memory and provides back-pressure.
    pub packet_pool_size: usize,
    /// A flow with no traffic in both directions for this long is dropped.
    pub flow_idle_timeout: Duration,
    /// A flow with no traffic in either direction for this long is dropped.
    pub flow_one_way_timeout: Duration,
    /// Bind to a per-client `127.x.y.z` address when the origin is on IPv4 loopback and
    /// no proxy protocol header carries the real client address.
    pub client_loopback_ip: bool,
}

impl Default for UdpSettings {
    fn default() -> Self {
        UdpSettings {
            new_client_ratelimit: 16,
            new_client_ratelimit_burst: 32,
            packet_pool_size: 8 * 1024,
            flow_idle_timeout: Duration::from_secs(60),
            flow_one_way_timeout: Duration::from_secs(90),
            client_loopback_ip: true,
        }
    }
}

const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(1);
/// Resend the session token this often while the channel is confirmed.
const ESTABLISH_INTERVAL: Duration = Duration::from_secs(10);
/// Resend cadence once the last confirmation is older than `ESTABLISH_STALE_AFTER`.
const ESTABLISH_RETRY_INTERVAL: Duration = Duration::from_secs(3);
const ESTABLISH_STALE_AFTER: Duration = Duration::from_secs(15);
/// Without a confirmation for this long, ask the control channel for fresh details.
const SESSION_STALE_AFTER: Duration = Duration::from_secs(6);
const SESSION_REQUEST_INTERVAL: Duration = Duration::from_secs(5);
const ORIGIN_CHANNEL_LEN: usize = 2048;

struct UdpSession {
    details: UdpChannelDetails,
    last_establish_sent: Instant,
    /// When the server last confirmed the channel.
    confirmed_at: Option<Instant>,
}

pub struct UdpTunnel {
    settings: UdpSettings,
    socket: DualStackUdpSocket,
    session: Option<UdpSession>,
    control: ControlHandle,
    lookup: Arc<OriginLookup>,
    stats: AgentStats,
    limiter: DefaultDirectRateLimiter,
    tunnel_pool: PacketPool,
    flows: FlowTable,
    origin_rx: mpsc::Receiver<OriginPacket>,
    /// Kept so `origin_rx` never observes a closed channel.
    _origin_tx: mpsc::Sender<OriginPacket>,
    last_session_request: Option<Instant>,
}

impl UdpTunnel {
    pub async fn new(
        settings: UdpSettings,
        lookup: Arc<OriginLookup>,
        stats: AgentStats,
        control: ControlHandle,
    ) -> std::io::Result<Self> {
        let socket = DualStackUdpSocket::new().await?;
        let (origin_tx, origin_rx) = mpsc::channel(ORIGIN_CHANNEL_LEN);
        let flows = FlowTable::new(
            PacketPool::new(settings.packet_pool_size),
            origin_tx.clone(),
            stats.clone(),
        );

        Ok(UdpTunnel {
            limiter: RateLimiter::direct(new_client_quota(&settings)),
            tunnel_pool: PacketPool::new(settings.packet_pool_size),
            settings,
            socket,
            session: None,
            control,
            lookup,
            stats,
            flows,
            origin_rx,
            _origin_tx: origin_tx,
            last_session_request: None,
        })
    }

    pub fn local_ip4_port(&self) -> Option<u16> {
        self.socket.local_ip4_port()
    }

    pub fn flow_details(&self) -> Vec<UdpFlowDetails> {
        self.flows.details()
    }

    /// Relays datagrams until cancelled. `sessions` delivers channel details from the
    /// control channel.
    pub async fn run(
        mut self,
        mut sessions: mpsc::Receiver<UdpChannelDetails>,
        cancel: CancellationToken,
    ) {
        let mut inbound = self.tunnel_pool.allocate().await;
        let mut next_maintenance = Instant::now();
        let mut sessions_open = true;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                received = self.socket.recv_from(inbound.capacity_mut()) => {
                    match received {
                        Ok((len, from)) => {
                            inbound.set_len(len);
                            let packet = std::mem::replace(&mut inbound, self.tunnel_pool.allocate().await);
                            self.handle_tunnel_packet(packet, from).await;
                        }
                        Err(error) => {
                            self.stats.udp().recv_failed.inc();
                            tracing::debug!(?error, "udp channel receive failed");
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }
                }
                Some(origin_packet) = self.origin_rx.recv() => {
                    self.handle_origin_packet(origin_packet).await;
                }
                details = sessions.recv(), if sessions_open => match details {
                    Some(details) => self.set_session(details).await,
                    None => sessions_open = false,
                },
                _ = sleep_until(next_maintenance) => {
                    next_maintenance = Instant::now() + MAINTENANCE_INTERVAL;
                    self.maintain().await;
                }
            }
        }
    }

    async fn set_session(&mut self, details: UdpChannelDetails) {
        let now = Instant::now();

        let (resend, confirmed_at, last_establish_sent) = match &self.session {
            None => (true, None, now),
            Some(current) if current.details != details => (true, None, now),
            Some(current) => {
                let stale = current
                    .confirmed_at
                    .is_none_or(|at| SESSION_REQUEST_INTERVAL <= now.saturating_duration_since(at));
                (stale, current.confirmed_at, current.last_establish_sent)
            }
        };

        tracing::debug!(tunnel = %details.tunnel_addr, resend, "udp session details updated");
        self.session = Some(UdpSession {
            details,
            last_establish_sent,
            confirmed_at,
        });

        if resend {
            self.send_establish().await;
        }
    }

    async fn send_establish(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.last_establish_sent = Instant::now();

        if let Err(error) = self
            .socket
            .send_to(&session.details.token, session.details.tunnel_addr)
            .await
        {
            self.stats.udp().tunnel_send_failed.inc();
            tracing::warn!(?error, "failed to send udp session token");
        }
    }

    async fn maintain(&mut self) {
        let now = Instant::now();

        if let Some(session) = &self.session {
            let confirmed_recently = session
                .confirmed_at
                .is_some_and(|at| now.saturating_duration_since(at) < ESTABLISH_STALE_AFTER);
            let interval = if confirmed_recently {
                ESTABLISH_INTERVAL
            } else {
                ESTABLISH_RETRY_INTERVAL
            };
            if interval <= now.saturating_duration_since(session.last_establish_sent) {
                self.send_establish().await;
            }
        }

        let session_stale = self.session.as_ref().is_none_or(|session| {
            session
                .confirmed_at
                .is_none_or(|at| SESSION_STALE_AFTER <= now.saturating_duration_since(at))
        });
        let can_request = self
            .last_session_request
            .is_none_or(|at| SESSION_REQUEST_INTERVAL <= now.saturating_duration_since(at));
        if session_stale && can_request {
            self.last_session_request = Some(now);
            self.control.request_udp_session();
        }

        let evicted = self.flows.evict_idle(
            now,
            self.settings.flow_idle_timeout,
            self.settings.flow_one_way_timeout,
        );
        if 0 < evicted {
            self.stats.set_active_udp(self.flows.len() as u32);
        }
    }

    async fn handle_tunnel_packet(&mut self, mut packet: Packet, from: SocketAddr) {
        let counters = self.stats.udp();
        let Some(session) = self.session.as_mut() else {
            counters.no_session.inc();
            return;
        };
        if from != session.details.tunnel_addr {
            counters.unexpected_source.inc();
            return;
        }

        let flow = match UdpFlow::from_tail(&packet) {
            Ok(flow) => flow,
            Err(Some(UDP_CHANNEL_ESTABLISH_ID)) => {
                if session.confirmed_at.is_none() {
                    tracing::info!(tunnel = %session.details.tunnel_addr, "udp channel established");
                }
                session.confirmed_at = Some(Instant::now());
                return;
            }
            Err(_) => {
                counters.invalid_packet.inc();
                return;
            }
        };

        let payload_len = packet.len() - flow.footer_len();
        packet.truncate(payload_len);
        self.deliver_to_origin(flow, packet).await;
    }

    async fn deliver_to_origin(&mut self, flow: UdpFlow, packet: Packet) {
        let now = Instant::now();
        let counters = self.stats.udp();

        let Some(extension) = flow.extension() else {
            counters.invalid_packet.inc();
            return;
        };
        let client = ClientKey {
            client_addr: flow.src(),
            tunnel_id: extension.tunnel_id.get(),
            port_offset: extension.port_offset,
        };
        let payload_len = packet.len() as u64;

        if let Some(existing) = self.flows.find_mut(&client) {
            existing.last_from_tunnel = now;
            existing.reply_flow = flow.flip();
            if existing
                .socket
                .send_to(&packet, existing.target)
                .await
                .is_err()
            {
                counters.origin_send_failed.inc();
            } else {
                self.stats.add_bytes_in(payload_len);
            }
            return;
        }

        if self.limiter.check().is_err() {
            counters.rate_limited.inc();
            return;
        }

        let Some(origin) = self.lookup.lookup(client.tunnel_id, Transport::Udp) else {
            counters.origin_not_found.inc();
            return;
        };
        let Some(target) = origin.resolve_local(client.port_offset).await else {
            counters.invalid_port_offset.inc();
            return;
        };

        let client_loopback_ip =
            self.settings.client_loopback_ip && origin.proxy_protocol.is_none();
        let socket = match lan::bind_udp(
            client_loopback_ip,
            client.client_addr,
            target,
            client.tunnel_id,
        )
        .await
        {
            Ok(socket) => Arc::new(socket),
            Err(error) => {
                counters.bind_failed.inc();
                tracing::error!(
                    ?error,
                    %target,
                    client = %client.client_addr,
                    tunnel_id = client.tunnel_id,
                    "failed to open local UDP socket for tunnel traffic"
                );
                return;
            }
        };

        tracing::debug!(client = ?client, %target, "new udp client");

        match origin.proxy_protocol {
            Some(ProxyProtocol::ProxyProtocolV2) => {
                let mut header = Vec::with_capacity(64);
                ProxyProtocolHeader::from_udp_flow(&flow)
                    .write_v2_udp(&mut header)
                    .expect("writing to a Vec cannot fail");
                if socket.send_to(&header, target).await.is_err() {
                    counters.origin_send_failed.inc();
                }
            }
            Some(ProxyProtocol::ProxyProtocolV1) => {
                counters.unsupported_proxy_protocol.inc();
            }
            None => {}
        }

        if socket.send_to(&packet, target).await.is_err() {
            counters.origin_send_failed.inc();
        } else {
            self.stats.add_bytes_in(payload_len);
        }

        self.flows.insert(client, socket, target, flow.flip(), now);
        self.stats.set_active_udp(self.flows.len() as u32);
    }

    async fn handle_origin_packet(&mut self, origin_packet: OriginPacket) {
        let OriginPacket {
            flow: key,
            from,
            mut packet,
        } = origin_packet;
        let counters = self.stats.udp();

        let Some(flow) = self.flows.get_mut(key) else {
            counters.stale_flow.inc();
            return;
        };
        if from != flow.target {
            counters.unexpected_source.inc();
            return;
        }
        flow.last_from_origin = Instant::now();
        let reply_flow = flow.reply_flow;

        let Some(session) = self.session.as_ref() else {
            counters.no_session.inc();
            return;
        };

        let payload_len = packet.len();
        let footer_len = reply_flow.footer_len();
        if packet.capacity() < payload_len + footer_len {
            counters.invalid_packet.inc();
            return;
        }
        reply_flow.write_to(&mut packet.capacity_mut()[payload_len..]);
        packet.set_len(payload_len + footer_len);

        if self
            .socket
            .send_to(&packet, session.details.tunnel_addr)
            .await
            .is_err()
        {
            counters.tunnel_send_failed.inc();
        } else {
            self.stats.add_bytes_out(payload_len as u64);
        }
    }
}

fn new_client_quota(settings: &UdpSettings) -> Quota {
    let rate = NonZeroU32::new(settings.new_client_ratelimit).unwrap_or_else(|| {
        tracing::warn!("udp new client rate limit of 0 clamped to 1");
        NonZeroU32::MIN
    });
    let burst = NonZeroU32::new(settings.new_client_ratelimit_burst).unwrap_or_else(|| {
        tracing::warn!("udp new client burst of 0 clamped to 1");
        NonZeroU32::MIN
    });
    Quota::per_second(rate).allow_burst(burst)
}
