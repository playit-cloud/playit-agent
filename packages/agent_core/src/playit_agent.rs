use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use tokio::sync::mpsc::{channel, error::TrySendError};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::agent_control::errors::SetupError;
use crate::agent_control::maintained_control::{MaintainedControl, TunnelControlEvent};
use crate::agent_control::{DualStackUdpSocket, GatewayAuth};
use crate::gateway::PlayitGateway;
use crate::network::origin_lookup::OriginLookup;
use crate::network::tcp::tcp_clients::{TcpClients, TcpService};
use crate::network::tcp::tcp_settings::TcpSettings;
use crate::network::udp::packets::Packets;
use crate::network::udp::udp_channel::{UdpChannel, UdpChannelTasks};
use crate::network::udp::udp_clients::UdpClients;
use crate::network::udp::udp_settings::UdpSettings;
use crate::stats::AgentStats;
use crate::utils::now_milli;
use playit_agent_proto::control_messages::UdpChannelDetails;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceExit {
    Completed,
    Cancelled,
    CleanEof,
    QueueClosed(String),
    Io { operation: String, message: String },
    DeadlineExceeded,
    Failed(String),
    Panicked(String),
}

impl ServiceExit {
    pub const fn is_fatal_child_failure(&self) -> bool {
        matches!(self, Self::DeadlineExceeded | Self::Panicked(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineService {
    Control,
    Tcp,
    TcpClaim,
    TcpPipe,
    Udp,
    UdpChannelSend,
    UdpChannelRecv,
    UdpFlow,
    Stats,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineExit {
    Cancelled,
    Completed,
    Service {
        service: EngineService,
        exit: ServiceExit,
    },
    ShutdownTimedOut {
        unfinished: Vec<EngineService>,
    },
    Panicked(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineLimits {
    pub packet_count: NonZeroUsize,
    pub shutdown_deadline: Duration,
    pub udp_flow_drain_deadline: Duration,
}

impl Default for EngineLimits {
    fn default() -> Self {
        Self {
            packet_count: NonZeroUsize::new(8 * 1024).expect("default packet count is non-zero"),
            shutdown_deadline: Duration::from_secs(5),
            udp_flow_drain_deadline: Duration::from_secs(4),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineLimitsError {
    ZeroPacketCount,
    ZeroShutdownDeadline,
    ZeroUdpFlowDrainDeadline,
}

impl EngineLimits {
    pub fn try_new(
        packet_count: usize,
        shutdown_deadline: Duration,
        udp_flow_drain_deadline: Duration,
    ) -> Result<Self, EngineLimitsError> {
        let packet_count =
            NonZeroUsize::new(packet_count).ok_or(EngineLimitsError::ZeroPacketCount)?;
        if shutdown_deadline.is_zero() {
            return Err(EngineLimitsError::ZeroShutdownDeadline);
        }
        if udp_flow_drain_deadline.is_zero() {
            return Err(EngineLimitsError::ZeroUdpFlowDrainDeadline);
        }
        Ok(Self {
            packet_count,
            shutdown_deadline,
            udp_flow_drain_deadline,
        })
    }
}

pub struct PlayitAgent {
    supervisor: EngineSupervisor,
    cancel: CancellationToken,
    stats: AgentStats,
}

#[derive(Clone, Debug, Default)]
pub struct PlayitAgentSettings {
    pub control_settings: ControlSettings,
    pub tcp_settings: TcpSettings,
    pub udp_settings: UdpSettings,
    pub limits: EngineLimits,
}

#[derive(Clone, Debug)]
pub struct ControlSettings {
    pub connect_timeout: Duration,
    pub retry_delay: Duration,
    pub event_queue_capacity: usize,
}

impl Default for ControlSettings {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            retry_delay: Duration::from_secs(2),
            event_queue_capacity: 1024,
        }
    }
}

pub struct EngineSupervisor {
    control: MaintainedControl<DualStackUdpSocket, GatewayAuth>,
    udp_clients: UdpClients,
    udp_channel: UdpChannel,
    udp_channel_tasks: UdpChannelTasks,
    tcp_clients: TcpClients,
    tcp_service: TcpService,
    cancel: CancellationToken,
    stats: AgentStats,
    limits: EngineLimits,
    control_event_queue_capacity: usize,
}

impl PlayitAgent {
    pub async fn new(
        settings: PlayitAgentSettings,
        lookup: Arc<OriginLookup>,
        gateway: Arc<dyn PlayitGateway>,
    ) -> Result<Self, SetupError> {
        let supervisor = EngineSupervisor::new(settings, lookup, gateway).await?;
        let cancel = supervisor.cancellation_token();
        let stats = supervisor.stats();
        Ok(Self {
            supervisor,
            cancel,
            stats,
        })
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn stats(&self) -> AgentStats {
        self.stats.clone()
    }

    pub fn into_supervisor(self) -> EngineSupervisor {
        self.supervisor
    }

    pub async fn run(self) -> EngineExit {
        self.supervisor.run().await
    }
}

impl EngineSupervisor {
    pub async fn new(
        settings: PlayitAgentSettings,
        lookup: Arc<OriginLookup>,
        gateway: Arc<dyn PlayitGateway>,
    ) -> Result<Self, SetupError> {
        let io = DualStackUdpSocket::new().await?;
        let auth = GatewayAuth::new(gateway);
        let control_event_queue_capacity = settings.control_settings.event_queue_capacity.max(1);
        let control = MaintainedControl::setup_with_policy(
            io,
            auth,
            settings.control_settings.connect_timeout,
            settings.control_settings.retry_delay,
        )
        .await?;

        let tunnel_packets = Packets::try_new(settings.limits.packet_count).map_err(|_| {
            SetupError::IoError(std::io::Error::other("invalid tunnel packet pool"))
        })?;
        let origin_packets = Packets::try_new(settings.limits.packet_count).map_err(|_| {
            SetupError::IoError(std::io::Error::other("invalid origin packet pool"))
        })?;
        let (udp_channel, udp_channel_tasks) = UdpChannel::new(tunnel_packets)
            .await
            .map_err(SetupError::IoError)?;

        let stats = AgentStats::new();
        let udp_clients = UdpClients::new(
            settings.udp_settings,
            lookup.clone(),
            origin_packets,
            stats.clone(),
        );
        let cancel = CancellationToken::new();
        let (tcp_clients, tcp_service) = TcpClients::new(
            settings.tcp_settings,
            lookup,
            stats.clone(),
            cancel.child_token(),
        );

        Ok(Self {
            control,
            udp_clients,
            udp_channel,
            udp_channel_tasks,
            tcp_clients,
            tcp_service,
            cancel,
            stats,
            limits: settings.limits,
            control_event_queue_capacity,
        })
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn stats(&self) -> AgentStats {
        self.stats.clone()
    }

    pub async fn run(self) -> EngineExit {
        let Self {
            mut control,
            mut udp_clients,
            mut udp_channel,
            udp_channel_tasks,
            tcp_clients,
            tcp_service,
            cancel,
            stats,
            limits,
            control_event_queue_capacity,
        } = self;

        let (udp_session_tx, mut udp_session_rx) = channel(control_event_queue_capacity);
        let udp_session_should_renew = Arc::new(AtomicBool::new(false));
        let mut tasks = JoinSet::new();
        let mut active = HashSet::new();

        active.insert(EngineService::Control);
        let control_cancel = cancel.child_token();
        let should_renew_udp = udp_session_should_renew.clone();
        tasks.spawn(async move {
            let mut last_control_addr_check = now_milli();
            loop {
                tokio::select! {
                    _ = control_cancel.cancelled() => {
                        return (EngineService::Control, ServiceExit::Cancelled);
                    }
                    _ = tokio::task::yield_now() => {}
                }

                if should_renew_udp.load(Ordering::Acquire) {
                    let Some(sent) = control_cancel
                        .run_until_cancelled(control.send_udp_session_auth(now_milli(), 5_000))
                        .await
                    else {
                        return (EngineService::Control, ServiceExit::Cancelled);
                    };
                    if sent {
                        tracing::debug!("requested new UDP session credentials");
                    }
                }

                let now = now_milli();
                if 30_000 < now.saturating_sub(last_control_addr_check) {
                    last_control_addr_check = now;
                    let reload = control.reload_control_addr(DualStackUdpSocket::new());
                    if let Some(Err(error)) = control_cancel.run_until_cancelled(reload).await {
                        tracing::error!(?error, "failed to reload control address");
                    }
                }

                let update = tokio::select! {
                    _ = control_cancel.cancelled() => {
                        return (EngineService::Control, ServiceExit::Cancelled);
                    }
                    update = control.update() => update,
                };

                match update {
                    Some(TunnelControlEvent::NewClient(new_client)) => {
                        if tcp_clients.handle_new_client(new_client).await.is_err() {
                            return (
                                EngineService::Control,
                                ServiceExit::QueueClosed("tcp admission".to_owned()),
                            );
                        }
                    }
                    Some(TunnelControlEvent::UdpChannelDetails(details)) => {
                        if let Err(exit) = enqueue_udp_session(&udp_session_tx, details) {
                            return (EngineService::Control, exit);
                        }
                    }
                    None => {}
                }
            }
        });

        active.insert(EngineService::Tcp);
        tasks.spawn(tcp_service.run());

        active.insert(EngineService::UdpChannelSend);
        active.insert(EngineService::UdpChannelRecv);
        let channel_cancel = cancel.child_token();
        tasks.spawn(async move { udp_channel_tasks.run(channel_cancel).await });

        active.insert(EngineService::Udp);
        let udp_cancel = cancel.child_token();
        tasks.spawn(async move {
            let mut next_clear = Instant::now() + Duration::from_secs(16);
            let (mut service, mut exit) = loop {
                tokio::select! {
                    _ = udp_cancel.cancelled() => break (EngineService::Udp, ServiceExit::Cancelled),
                    _ = tokio::task::yield_now() => {}
                }

                tokio::select! {
                    _ = udp_cancel.cancelled() => break (EngineService::Udp, ServiceExit::Cancelled),
                    recv = udp_clients.recv_origin_packet() => {
                        let recv = match recv {
                            Ok(recv) => recv,
                            Err(exit) => break (EngineService::Udp, exit),
                        };
                        let Some((flow, packet)) = udp_clients.dispatch_origin_packet(now_milli(), recv).await else {
                            continue;
                        };
                        if udp_channel.send(flow, packet).await.is_err() {
                            break (EngineService::Udp, ServiceExit::QueueClosed("udp channel send".to_owned()));
                        }
                    }
                    recv = udp_channel.recv() => {
                        let (flow, packet) = match recv {
                            Ok(packet) => packet,
                            Err(_) => break (EngineService::Udp, ServiceExit::QueueClosed("udp channel receive".to_owned())),
                        };
                        udp_clients.handle_tunneled_packet(now_milli(), flow, packet).await;
                    }
                    session = udp_session_rx.recv() => {
                        let Some(session) = session else {
                            break (EngineService::Udp, ServiceExit::QueueClosed("udp session".to_owned()));
                        };
                        if udp_channel.update_session(session).await.is_err() {
                            break (EngineService::Udp, ServiceExit::QueueClosed("udp channel session".to_owned()));
                        }
                    }
                    _ = tokio::time::sleep_until(next_clear) => {
                        next_clear = Instant::now() + Duration::from_secs(16);
                        if let Err(exit) = udp_clients.clear_old(now_milli()).await {
                            break (EngineService::UdpFlow, exit);
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }

                let renew = udp_channel
                    .time_since_established()
                    .is_none_or(|since| Duration::from_secs(6) <= since);
                udp_session_should_renew.store(renew, Ordering::Release);
            };

            let drain_exit = udp_clients.shutdown(limits.udp_flow_drain_deadline).await;
            if matches!(drain_exit, ServiceExit::DeadlineExceeded) {
                service = EngineService::UdpFlow;
                exit = drain_exit;
            }
            (service, exit)
        });

        active.insert(EngineService::Stats);
        let stats_cancel = cancel.child_token();
        tasks.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    _ = stats_cancel.cancelled() => {
                        return (EngineService::Stats, ServiceExit::Cancelled);
                    }
                    _ = tick.tick() => {
                        let _ = stats.snapshot();
                    }
                }
            }
        });

        let root_exit = tokio::select! {
            biased;
            _ = cancel.cancelled() => EngineExit::Cancelled,
            result = tasks.join_next() => match result {
                Some(Ok((service, exit))) => {
                    active.remove(&service);
                    if matches!(service, EngineService::TcpClaim | EngineService::TcpPipe) {
                        active.remove(&EngineService::Tcp);
                    }
                    if matches!(service, EngineService::UdpFlow) {
                        active.remove(&EngineService::Udp);
                    }
                    if matches!(service, EngineService::UdpChannelSend | EngineService::UdpChannelRecv) {
                        active.remove(&EngineService::UdpChannelSend);
                        active.remove(&EngineService::UdpChannelRecv);
                    }
                    EngineExit::Service { service, exit }
                }
                Some(Err(error)) => EngineExit::Panicked(error.to_string()),
                None => EngineExit::Completed,
            },
        };

        cancel.cancel();
        let deadline = Instant::now() + limits.shutdown_deadline;
        while !tasks.is_empty() {
            let next = tokio::time::timeout_at(deadline, tasks.join_next()).await;
            match next {
                Ok(Some(Ok((service, _)))) => {
                    active.remove(&service);
                    if matches!(service, EngineService::TcpClaim | EngineService::TcpPipe) {
                        active.remove(&EngineService::Tcp);
                    }
                    if matches!(service, EngineService::UdpFlow) {
                        active.remove(&EngineService::Udp);
                    }
                    if matches!(
                        service,
                        EngineService::UdpChannelSend | EngineService::UdpChannelRecv
                    ) {
                        active.remove(&EngineService::UdpChannelSend);
                        active.remove(&EngineService::UdpChannelRecv);
                    }
                }
                Ok(Some(Err(_))) => {}
                Ok(None) => break,
                Err(_) => {
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    return EngineExit::ShutdownTimedOut {
                        unfinished: active.into_iter().collect(),
                    };
                }
            }
        }
        root_exit
    }
}

fn enqueue_udp_session(
    sender: &tokio::sync::mpsc::Sender<UdpChannelDetails>,
    details: UdpChannelDetails,
) -> Result<(), ServiceExit> {
    match sender.try_send(details) {
        Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
        Err(TrySendError::Closed(_)) => Err(ServiceExit::QueueClosed("udp session".to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn udp_details() -> UdpChannelDetails {
        UdpChannelDetails {
            tunnel_addr: "127.0.0.1:1".parse().expect("fixture address"),
            token: Arc::new(vec![1]),
        }
    }

    #[test]
    fn engine_limits_reject_zero_bounds() {
        assert_eq!(
            EngineLimits::try_new(0, Duration::from_secs(1), Duration::from_secs(1)),
            Err(EngineLimitsError::ZeroPacketCount)
        );
        assert_eq!(
            EngineLimits::try_new(1, Duration::ZERO, Duration::from_secs(1)),
            Err(EngineLimitsError::ZeroShutdownDeadline)
        );
        assert_eq!(
            EngineLimits::try_new(1, Duration::from_secs(1), Duration::ZERO),
            Err(EngineLimitsError::ZeroUdpFlowDrainDeadline)
        );
    }

    #[test]
    fn control_queue_closure_has_a_typed_exit() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        drop(receiver);
        assert_eq!(
            enqueue_udp_session(&sender, udp_details()),
            Err(ServiceExit::QueueClosed("udp session".to_owned()))
        );
    }

    #[test]
    fn control_queue_overload_stays_bounded() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        enqueue_udp_session(&sender, udp_details()).expect("first update queued");
        enqueue_udp_session(&sender, udp_details()).expect("overload is coalesced");
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn ordinary_connection_io_is_not_a_service_failure() {
        assert!(
            !ServiceExit::Io {
                operation: "read TCP stream".to_owned(),
                message: "connection reset".to_owned(),
            }
            .is_fatal_child_failure()
        );
        assert!(ServiceExit::Panicked("fixture".to_owned()).is_fatal_child_failure());
        assert!(ServiceExit::DeadlineExceeded.is_fatal_child_failure());
    }
}
