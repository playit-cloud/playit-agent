//! Control channel to the tunnel server.
//!
//! [`ControlChannel`] owns one [`ControlLink`] and the registered [`Session`] on it and
//! keeps both healthy: it pings every second, sends keep-alives before the session
//! expires, re-registers when the server forgets the session or our observed address
//! changes, reconnects when pongs stop, and follows routing changes from the API.
//! Everything the rest of the agent needs from the server arrives as a
//! [`ControlEvent`] from [`ControlChannel::next_event`].

pub mod api;
pub mod link;
pub mod socket;

use std::{net::SocketAddr, time::Duration};

use playit_agent_proto::{
    control_feed::{ControlFeed, NewClient},
    control_messages::{ControlRequest, ControlResponse, Ping, Pong, UdpChannelDetails},
};
use tokio::{
    sync::mpsc,
    task::{JoinError, JoinHandle},
    time::{Instant, sleep, sleep_until, timeout},
};

pub use api::{ControlApi, PlayitControlApi};
pub use link::{ControlLink, Session};
pub use socket::DualStackUdpSocket;

use crate::{error::SetupError, util::now_milli};

#[derive(Debug, Clone)]
pub struct ControlSettings {
    pub ping_interval: Duration,
    /// Without a pong for this long the server is considered unreachable and the channel
    /// reconnects (re-probing every known control address).
    pub pong_timeout: Duration,
    pub keepalive_interval: Duration,
    /// Keep-alive cadence once the session is within `expiring_soon` of expiry.
    pub keepalive_interval_expiring: Duration,
    pub expiring_soon: Duration,
    /// How often the API is asked for control addresses to detect routing changes.
    pub routing_check_interval: Duration,
    /// Bound on probing addresses and on registering.
    pub connect_timeout: Duration,
    pub api_timeout: Duration,
    pub max_recovery_backoff: Duration,
}

impl Default for ControlSettings {
    fn default() -> Self {
        ControlSettings {
            ping_interval: Duration::from_secs(1),
            pong_timeout: Duration::from_secs(6),
            keepalive_interval: Duration::from_secs(60),
            keepalive_interval_expiring: Duration::from_secs(10),
            expiring_soon: Duration::from_secs(30),
            routing_check_interval: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            api_timeout: Duration::from_secs(5),
            max_recovery_backoff: Duration::from_secs(30),
        }
    }
}

#[derive(Debug)]
pub enum ControlEvent {
    /// A client connected to a TCP tunnel and is waiting to be claimed.
    NewClient(NewClient),
    /// Where and how to authenticate the UDP data channel.
    UdpSession(UdpChannelDetails),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCommand {
    /// Ask the server for (new) UDP session details.
    RequestUdpSession,
}

/// Lets other parts of the agent talk to the control channel from another task.
#[derive(Clone, Debug)]
pub struct ControlHandle {
    tx: mpsc::Sender<ControlCommand>,
}

impl ControlHandle {
    pub fn new(tx: mpsc::Sender<ControlCommand>) -> Self {
        ControlHandle { tx }
    }

    /// Requests UDP session details. Coalesced by the channel; never blocks.
    pub fn request_udp_session(&self) {
        let _ = self.tx.try_send(ControlCommand::RequestUdpSession);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recovery {
    /// The server is reachable, only the registration needs redoing.
    Register,
    /// The server stopped answering; probe every known address again.
    Reconnect,
}

struct RoutingProbe {
    addresses: Vec<SocketAddr>,
    /// A link to the new preferred server, if the address list changed and one answered.
    link: Option<ControlLink>,
}

pub struct ControlChannel<A: ControlApi> {
    api: A,
    settings: ControlSettings,
    link: ControlLink,
    session: Option<Session>,
    recovery: Recovery,
    recovery_failures: u32,
    addresses: Vec<SocketAddr>,
    commands: mpsc::Receiver<ControlCommand>,
    handle: ControlHandle,
    rtt_ms: Option<u32>,
    last_pong_at: Instant,
    next_ping_at: Instant,
    last_keepalive_at: Instant,
    next_routing_check_at: Instant,
    routing_probe: Option<JoinHandle<Result<RoutingProbe, SetupError>>>,
    udp_session_wanted: bool,
}

impl<A: ControlApi> ControlChannel<A> {
    /// Fetches control addresses, connects to the first that answers and registers.
    pub async fn connect(api: A, settings: ControlSettings) -> Result<Self, SetupError> {
        let addresses = timeout(settings.api_timeout, api.control_addresses())
            .await
            .map_err(|_| SetupError::Timeout("fetch control addresses"))??;
        if addresses.is_empty() {
            return Err(SetupError::NoControlAddresses);
        }

        let socket = DualStackUdpSocket::new().await?;
        let mut link = timeout(
            settings.connect_timeout,
            ControlLink::probe(socket, &addresses),
        )
        .await
        .map_err(|_| SetupError::Timeout("probe control addresses"))??;

        let session = timeout(
            settings.connect_timeout,
            register_with_retry(&mut link, &api),
        )
        .await
        .map_err(|_| SetupError::Timeout("register"))??;

        tracing::info!(remote = %link.remote(), session = ?session.id, "control channel registered");

        let (tx, commands) = mpsc::channel(16);
        let now = Instant::now();

        Ok(ControlChannel {
            api,
            next_routing_check_at: now + settings.routing_check_interval,
            settings,
            link,
            session: Some(session),
            recovery: Recovery::Register,
            recovery_failures: 0,
            addresses,
            commands,
            handle: ControlHandle::new(tx),
            rtt_ms: None,
            last_pong_at: now,
            next_ping_at: now,
            last_keepalive_at: now,
            routing_probe: None,
            udp_session_wanted: false,
        })
    }

    pub fn handle(&self) -> ControlHandle {
        self.handle.clone()
    }

    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    pub fn remote(&self) -> SocketAddr {
        self.link.remote()
    }

    pub fn latest_pong(&self) -> &Pong {
        self.link.pong()
    }

    pub fn rtt_ms(&self) -> Option<u32> {
        self.rtt_ms
    }

    /// Drives the channel until the server produces something the agent must act on.
    ///
    /// Cancel safe: dropping the future between events leaves the channel consistent.
    pub async fn next_event(&mut self) -> ControlEvent {
        loop {
            if self.session.is_none() {
                self.recover().await;
                continue;
            }

            tokio::select! {
                received = self.link.recv() => match received {
                    Ok(feed) => {
                        if let Some(event) = self.handle_feed(feed) {
                            return event;
                        }
                    }
                    Err(error) => {
                        tracing::debug!(?error, "control receive failed");
                        sleep(Duration::from_millis(10)).await;
                    }
                },
                command = self.commands.recv() => {
                    if let Some(command) = command {
                        self.handle_command(command);
                    }
                }
                finished = async { self.routing_probe.as_mut().expect("guarded by precondition").await },
                    if self.routing_probe.is_some() =>
                {
                    self.routing_probe = None;
                    self.apply_routing_probe(finished).await;
                }
                _ = sleep_until(self.next_ping_at) => self.tick().await,
            }
        }
    }

    fn handle_command(&mut self, command: ControlCommand) {
        match command {
            ControlCommand::RequestUdpSession => self.udp_session_wanted = true,
        }
    }

    fn handle_feed(&mut self, feed: ControlFeed) -> Option<ControlEvent> {
        let response = match feed {
            ControlFeed::NewClient(client) => return Some(ControlEvent::NewClient(client)),
            ControlFeed::Response(response) => response,
        };

        match response.content {
            ControlResponse::Pong(pong) => self.handle_pong(pong),
            ControlResponse::AgentRegistered(registered) => {
                if let Some(session) = self.session.as_mut() {
                    session.apply_registered(&registered, self.link.pong());
                }
            }
            ControlResponse::UdpChannelDetails(details) => {
                return Some(ControlEvent::UdpSession(details));
            }
            ControlResponse::Unauthorized => {
                tracing::warn!("control session no longer authorized, registering again");
                self.session = None;
                self.recovery = Recovery::Register;
            }
            other => tracing::debug!(?other, "unhandled control response"),
        }

        None
    }

    fn handle_pong(&mut self, pong: Pong) {
        let now = Instant::now();
        self.last_pong_at = now;

        let rtt = now_milli().saturating_sub(pong.request_now);
        self.rtt_ms = Some(rtt.min(u32::MAX as u64) as u32);

        let Some(session) = self.session.as_mut() else {
            return;
        };
        if pong.request_now < session.registered_at {
            return;
        }

        if pong.client_addr != session.client_addr || pong.tunnel_addr != session.tunnel_addr {
            tracing::warn!(
                old_client = %session.client_addr,
                new_client = %pong.client_addr,
                old_tunnel = %session.tunnel_addr,
                new_tunnel = %pong.tunnel_addr,
                "observed address changed, registering again"
            );
            self.session = None;
            self.recovery = Recovery::Register;
            return;
        }

        match pong.session_expire_at {
            Some(expires_at) => {
                session.expires_at = link::local_expiry(expires_at, pong.server_now, rtt);
            }
            None => {
                tracing::warn!("server does not know our session, registering again");
                self.session = None;
                self.recovery = Recovery::Register;
            }
        }
    }

    async fn tick(&mut self) {
        let now = Instant::now();
        self.next_ping_at = now + self.settings.ping_interval;

        let Some(session) = self.session.as_ref() else {
            return;
        };

        if self.settings.pong_timeout <= now.duration_since(self.last_pong_at) {
            tracing::warn!(remote = %self.link.remote(), "no pong from control server, reconnecting");
            self.session = None;
            self.recovery = Recovery::Reconnect;
            return;
        }

        let session_id = session.id.clone();
        let until_expiry = session.expires_at.saturating_duration_since(now);

        self.send_or_log(ControlRequest::Ping(Ping {
            now: now_milli(),
            current_ping: self.rtt_ms,
            session_id: Some(session_id.clone()),
        }))
        .await;

        let keepalive_interval = if until_expiry <= self.settings.expiring_soon {
            self.settings.keepalive_interval_expiring
        } else {
            self.settings.keepalive_interval
        };
        if keepalive_interval <= now.duration_since(self.last_keepalive_at) {
            self.last_keepalive_at = now;
            tracing::debug!(?until_expiry, "sending keep alive");
            self.send_or_log(ControlRequest::AgentKeepAlive(session_id.clone()))
                .await;
        }

        if self.udp_session_wanted {
            self.udp_session_wanted = false;
            tracing::debug!("requesting udp session details");
            self.send_or_log(ControlRequest::SetupUdpChannel(session_id))
                .await;
        }

        if self.routing_probe.is_none() && self.next_routing_check_at <= now {
            self.next_routing_check_at = now + self.settings.routing_check_interval;
            self.routing_probe = Some(tokio::spawn(routing_probe(
                self.api.clone(),
                self.addresses.clone(),
                self.settings.clone(),
            )));
        }
    }

    async fn send_or_log(&mut self, request: ControlRequest) {
        if let Err(error) = self.link.send_request(request).await {
            tracing::warn!(?error, "failed to send control request");
        }
    }

    /// Re-establishes a session, sleeping with backoff on failure. Returns after one
    /// attempt either way so the caller can observe cancellation between attempts.
    async fn recover(&mut self) {
        let result = match self.recovery {
            Recovery::Register => {
                timeout(self.settings.connect_timeout, self.link.register(&self.api))
                    .await
                    .map_err(|_| SetupError::Timeout("register"))
                    .and_then(|res| res)
            }
            Recovery::Reconnect => self.reconnect().await,
        };

        match result {
            Ok(session) => {
                tracing::info!(remote = %self.link.remote(), session = ?session.id, "control session registered");
                let now = Instant::now();
                self.session = Some(session);
                self.recovery = Recovery::Register;
                self.recovery_failures = 0;
                self.last_pong_at = now;
                self.next_ping_at = now;
                self.last_keepalive_at = now;
                self.rtt_ms = None;
            }
            Err(SetupError::AddressChanged) => {
                // The link now holds the fresh addresses; sign for those right away.
                tracing::debug!("observed address changed during register, retrying");
            }
            Err(error) => {
                self.recovery_failures += 1;
                if self.recovery == Recovery::Register && 3 <= self.recovery_failures {
                    self.recovery = Recovery::Reconnect;
                }

                let backoff =
                    recovery_backoff(self.recovery_failures, self.settings.max_recovery_backoff);
                tracing::error!(
                    ?error,
                    failures = self.recovery_failures,
                    next = ?self.recovery,
                    "control recovery failed, retrying in {backoff:?}"
                );
                sleep(backoff).await;
            }
        }
    }

    async fn reconnect(&mut self) -> Result<Session, SetupError> {
        match timeout(self.settings.api_timeout, self.api.control_addresses()).await {
            Ok(Ok(addresses)) if !addresses.is_empty() => self.addresses = addresses,
            Ok(Ok(_)) => tracing::warn!("api returned no control addresses, reusing last known"),
            Ok(Err(error)) => tracing::warn!(
                ?error,
                "failed to fetch control addresses, reusing last known"
            ),
            Err(_) => tracing::warn!("timeout fetching control addresses, reusing last known"),
        }

        let socket = DualStackUdpSocket::new().await?;
        self.link = timeout(
            self.settings.connect_timeout,
            ControlLink::probe(socket, &self.addresses),
        )
        .await
        .map_err(|_| SetupError::Timeout("probe control addresses"))??;

        timeout(self.settings.connect_timeout, self.link.register(&self.api))
            .await
            .map_err(|_| SetupError::Timeout("register"))?
    }

    async fn apply_routing_probe(
        &mut self,
        finished: Result<Result<RoutingProbe, SetupError>, JoinError>,
    ) {
        let probe = match finished {
            Ok(Ok(probe)) => probe,
            Ok(Err(error)) => {
                tracing::warn!(?error, "routing check failed");
                return;
            }
            Err(error) => {
                tracing::error!(?error, "routing check task failed");
                return;
            }
        };

        self.addresses = probe.addresses;
        let Some(mut link) = probe.link else {
            return;
        };

        let current = self.link.pong();
        let candidate = link.pong();
        if current.tunnel_addr == candidate.tunnel_addr
            && current.client_addr.ip() == candidate.client_addr.ip()
        {
            tracing::debug!("routing changed but resolves to the same server, keeping connection");
            return;
        }

        match timeout(self.settings.connect_timeout, link.register(&self.api)).await {
            Ok(Ok(session)) => {
                tracing::info!(old = %self.link.remote(), new = %link.remote(), "switched control server");
                let now = Instant::now();
                self.link = link;
                self.session = Some(session);
                self.last_pong_at = now;
                self.next_ping_at = now;
                self.last_keepalive_at = now;
                self.rtt_ms = None;
            }
            Ok(Err(error)) => tracing::warn!(?error, "failed to register with new control server"),
            Err(_) => tracing::warn!("timeout registering with new control server"),
        }
    }
}

fn recovery_backoff(failures: u32, max: Duration) -> Duration {
    let exp = failures.saturating_sub(1).min(16);
    Duration::from_secs(1u64 << exp).min(max)
}

async fn register_with_retry<A: ControlApi>(
    link: &mut ControlLink,
    api: &A,
) -> Result<Session, SetupError> {
    for _ in 0..3 {
        match link.register(api).await {
            Err(SetupError::AddressChanged | SetupError::RegisterUnauthorized) => continue,
            result => return result,
        }
    }
    link.register(api).await
}

async fn routing_probe<A: ControlApi>(
    api: A,
    current: Vec<SocketAddr>,
    settings: ControlSettings,
) -> Result<RoutingProbe, SetupError> {
    let addresses = timeout(settings.api_timeout, api.control_addresses())
        .await
        .map_err(|_| SetupError::Timeout("fetch control addresses"))??;

    if addresses.is_empty() {
        return Err(SetupError::NoControlAddresses);
    }
    if addresses == current {
        return Ok(RoutingProbe {
            addresses,
            link: None,
        });
    }

    tracing::info!(?addresses, "control addresses changed, probing");
    let socket = DualStackUdpSocket::new().await?;
    let link = timeout(
        settings.connect_timeout,
        ControlLink::probe(socket, &addresses),
    )
    .await
    .map_err(|_| SetupError::Timeout("probe control addresses"))??;

    Ok(RoutingProbe {
        addresses,
        link: Some(link),
    })
}
