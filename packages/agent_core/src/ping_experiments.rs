use std::{collections::HashMap, net::SocketAddr, sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc}, time::Duration};

use message_encoding::MessageEncoding;
use playit_agent_proto::{control_messages::{ControlRequest, ControlResponse, ExperimentResultEntry, Ping}, rpc::ControlRpcMessage};
use tokio::sync::{mpsc::{channel, Receiver, Sender}, RwLock};

use crate::{api::api::{AgentRoutingTarget, PingExperimentRequest, PlayitPop, PlayitRegion}, utils::now_milli};

use crate::agent_control::{DualStackUdpSocket, PacketIO};

#[derive(Clone)]
pub struct PingExperiments {
    inner: Arc<Inner>,
}

struct Inner {
    run: AtomicBool,
    socket: DualStackUdpSocket,
    experiments: RwLock<Vec<Experiment>>,
    sender: Sender<ExperimentResultEntry>
}

struct Experiment {
    request: PingExperimentRequest,
    last_send: AtomicU64,
}

impl PingExperiments {
    pub async fn new(target_route_filter: Arc<RwLock<AgentRoutingTarget>>) -> std::io::Result<(Self, Receiver<ExperimentResultEntry>)> {
        let socket = DualStackUdpSocket::new().await?;
        let (tx, rx) = channel(2048);

        let inner = Arc::new(Inner {
            run: AtomicBool::new(true),
            socket,
            experiments: RwLock::new(vec![]),
            sender: tx,
        });

        tokio::spawn(PingSender { inner: inner.clone(), buffer: Vec::with_capacity(2048), target_route_filter }.start());
        tokio::spawn(PingReceiver { inner: inner.clone() }.start());

        Ok((
            PingExperiments { inner },
            rx
        ))
    }

    pub async fn update_experiments(&self, value: Vec<PingExperimentRequest>) {
        let mut lock = self.inner.experiments.write().await;

        let mut lookup = HashMap::new();
        for exp in lock.drain(..) {
            lookup.insert(exp.request.id, exp);
        }

        for request in value {
            match lookup.remove_entry(&request.id) {
                Some((_, mut data)) => {
                    data.request = request;
                    lock.push(data);
                }
                None => {
                    lock.push(Experiment {
                        request,
                        last_send: AtomicU64::new(0),
                    });
                }
            }
        }
    }
}

impl Drop for PingExperiments {
    fn drop(&mut self) {
        self.inner.run.store(false, Ordering::Release);
    }
}

struct PingSender {
    inner: Arc<Inner>,
    buffer: Vec<u8>,
    target_route_filter: Arc<RwLock<AgentRoutingTarget>>,
}

struct PingReceiver {
    inner: Arc<Inner>,
}

impl PingSender {
    async fn start(mut self) {
        while self.inner.run.load(Ordering::Relaxed) {
            self.run().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn run(&mut self) {
        let routing = {
            let lock = self.target_route_filter.read().await;
            AgentRoutingTarget::clone(&*lock)
        };

        let mut packets = Vec::new();

        /* prepare ping packest */
        {
            let lock = self.inner.experiments.read().await;
            let now = now_milli();

            for item in lock.iter() {
                let last_send = item.last_send.load(Ordering::Relaxed);
                let time_since_last_send = now.max(last_send) - last_send;

                if item.request.frequency_ms < time_since_last_send {
                    continue;
                }

                /* skip experiment if not relavent to us */
                if let Some(required_routing) = item.request.run_condition.as_ref().and_then(|v| v.routing_target.as_ref()) {
                    match (&routing, required_routing) {
                        (AgentRoutingTarget::Pop(a), AgentRoutingTarget::Pop(b)) if a != b => continue,
                        (AgentRoutingTarget::Pop(pop), AgentRoutingTarget::Region(region)) if !pop_region(pop).eq(region) => continue,
                        (AgentRoutingTarget::Automatic, _) => continue,
                        _ => {}
                    }
                }

                let pkt_start = self.buffer.len();

                let res = ControlRpcMessage {
                    request_id: item.request.id,
                    content: ControlRequest::Ping(Ping {
                        now,
                        current_ping: None,
                        session_id: None,
                    }),
                }.write_to(&mut self.buffer);

                if let Err(error) = res {
                    tracing::error!(?error, "failed to write ping message");
                    self.buffer.truncate(pkt_start);
                    continue;
                }

                item.last_send.store(now, Ordering::Relaxed);
                packets.push((pkt_start, self.buffer.len(), item.request.taret_ip, item.request.target_port));
            }
        }

        let mut error_count = 0;
        for (pkt_start, pkt_end, target_ip, target_port) in packets {
            let slice = &self.buffer[pkt_start..pkt_end];

            if let Err(error) = self.inner.socket.send_to(slice, SocketAddr::new(target_ip, target_port)).await {
                error_count += 1;
                if error_count <= 3 {
                    tracing::error!(?error, "failed to send ping");
                }
                continue;
            }

            error_count = 0;
        }
    }
}

impl PingReceiver {
    async fn start(self) {
        let mut buffer = Vec::with_capacity(2048);
        buffer.resize(2048, 0u8);

        let mut parse_errors = 0u64;
        let mut io_errors = 0u64;

        while self.inner.run.load(Ordering::Relaxed) {
            let (bytes, source) = match tokio::time::timeout(Duration::from_millis(100), self.inner.socket.recv_from(&mut buffer)).await {
                Ok(Ok(v)) => {
                    io_errors /= 2;
                    v
                },
                Ok(Err(error)) => {
                    io_errors += 1;
                    if io_errors <= 3 {
                        tracing::error!(?error, "failed to read from socket for ping experiment");
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                Err(_) => continue,
            };

            let mut slice = &buffer[..bytes];
            let response = match ControlRpcMessage::<ControlResponse>::read_from(&mut slice) {
                Ok(res) => {
                    parse_errors /= 2;
                    res
                },
                Err(error) => {
                    parse_errors += 1;
                    if parse_errors <= 3 {
                        tracing::error!(?error, ?source, "failed to parse packet response");
                    }
                    continue;
                }
            };
            
            let experiment_id = response.request_id;
            let ControlResponse::Pong(pong) = response.content else { continue; };

            let latency_ms = now_milli() - pong.request_now;
            if 15_000 < latency_ms {
                continue;
            }

            let lock = self.inner.experiments.read().await;
            let Some(exp) = lock.iter().find(|e| e.request.id == experiment_id) else { continue };

            /* ensure source of pong is valid */
            if exp.request.taret_ip != source.ip() || exp.request.alt_recv_ip.map(|v| v == source.ip()).unwrap_or(false) {
                continue;
            }

            let _ = self.inner.sender.try_send(ExperimentResultEntry {
                id: experiment_id,
                latency_ms: latency_ms as u32,
                dc_id: pong.data_center_id,
            });
        }
    }
}

fn pop_region(pop: &PlayitPop) -> PlayitRegion {
    match pop {
        PlayitPop::Any => PlayitRegion::GlobalAnycast,

        PlayitPop::UsLosAngeles => PlayitRegion::NorthAmerica,
        PlayitPop::UsSeattle => PlayitRegion::NorthAmerica,
        PlayitPop::UsDallas => PlayitRegion::NorthAmerica,
        PlayitPop::UsMiami => PlayitRegion::NorthAmerica,
        PlayitPop::UsChicago => PlayitRegion::NorthAmerica,
        PlayitPop::UsNewJersey => PlayitRegion::NorthAmerica,
        PlayitPop::CanadaToronto => PlayitRegion::NorthAmerica,
        PlayitPop::Mexico => PlayitRegion::NorthAmerica,
        PlayitPop::UsNewYork => PlayitRegion::NorthAmerica,
        PlayitPop::UsDenver => PlayitRegion::NorthAmerica,

        PlayitPop::Spain => PlayitRegion::Europe,
        PlayitPop::London => PlayitRegion::Europe,
        PlayitPop::Germany => PlayitRegion::Europe,
        PlayitPop::Poland => PlayitRegion::Europe,
        PlayitPop::Sweden => PlayitRegion::Europe,
        PlayitPop::Israel => PlayitRegion::Europe,
        PlayitPop::Romania => PlayitRegion::Europe,

        PlayitPop::IndiaDelhi => PlayitRegion::India,
        PlayitPop::IndiaMumbai => PlayitRegion::India,
        PlayitPop::IndiaBangalore => PlayitRegion::India,

        PlayitPop::Singapore => PlayitRegion::Asia,
        PlayitPop::Tokyo => PlayitRegion::Asia,
        PlayitPop::Sydney => PlayitRegion::Asia,

        PlayitPop::BrazilSaoPaulo => PlayitRegion::SouthAmerica,
        PlayitPop::SantiagoChile => PlayitRegion::SouthAmerica,
    }
}