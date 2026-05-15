use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(super) const TCP_UPLOAD_QOS_SLICE_SIZE: usize = 2048;
const TCP_UPLOAD_QOS_CHANNEL_SIZE: usize = 1024;

#[derive(Clone)]
pub(super) struct TcpUploadFairness {
    events_tx: mpsc::Sender<Event>,
    next_flow_id: Arc<AtomicU64>,
}

pub(super) struct TcpUploadFlow {
    id: u64,
    events_tx: mpsc::Sender<Event>,
}

struct PendingRequest {
    grant: oneshot::Sender<bool>,
}

struct FlowState {
    pending: Option<PendingRequest>,
    queued: bool,
}

enum Event {
    Request {
        flow_id: u64,
        _bytes: usize,
        grant: oneshot::Sender<bool>,
    },
    Unregister {
        flow_id: u64,
    },
}

struct Worker {
    events: mpsc::Receiver<Event>,
    cancel: CancellationToken,
    flows: HashMap<u64, FlowState>,
    ready: VecDeque<u64>,
}

impl TcpUploadFairness {
    pub(super) fn new(cancel: CancellationToken) -> Self {
        let (events_tx, events_rx) = mpsc::channel(TCP_UPLOAD_QOS_CHANNEL_SIZE);

        tokio::spawn(
            Worker {
                events: events_rx,
                cancel,
                flows: HashMap::new(),
                ready: VecDeque::new(),
            }
            .start(),
        );

        Self {
            events_tx,
            next_flow_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(super) fn register(&self) -> TcpUploadFlow {
        TcpUploadFlow {
            id: self.next_flow_id.fetch_add(1, Ordering::Relaxed),
            events_tx: self.events_tx.clone(),
        }
    }
}

impl TcpUploadFlow {
    pub(super) async fn acquire(&self, bytes: usize, cancel: &CancellationToken) -> bool {
        if bytes == 0 {
            return true;
        }

        let (grant_tx, grant_rx) = oneshot::channel();
        let event = Event::Request {
            flow_id: self.id,
            _bytes: bytes,
            grant: grant_tx,
        };

        let send_res = tokio::select! {
            _ = cancel.cancelled() => return false,
            res = self.events_tx.send(event) => res,
        };

        if send_res.is_err() {
            return false;
        }

        tokio::select! {
            _ = cancel.cancelled() => false,
            res = grant_rx => res.unwrap_or(false),
        }
    }
}

impl Drop for TcpUploadFlow {
    fn drop(&mut self) {
        let _ = self
            .events_tx
            .try_send(Event::Unregister { flow_id: self.id });
    }
}

impl Worker {
    async fn start(mut self) {
        loop {
            let event = tokio::select! {
                _ = self.cancel.cancelled() => break,
                event = self.events.recv() => {
                    let Some(event) = event else {
                        break;
                    };
                    event
                }
            };

            self.handle_event(event);

            while let Ok(event) = self.events.try_recv() {
                self.handle_event(event);
            }

            while self.grant_next() {
                while let Ok(event) = self.events.try_recv() {
                    self.handle_event(event);
                }
            }
        }

        for flow in self.flows.into_values() {
            if let Some(pending) = flow.pending {
                let _ = pending.grant.send(false);
            }
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Request { flow_id, grant, .. } => {
                let flow = self.flows.entry(flow_id).or_insert(FlowState {
                    pending: None,
                    queued: false,
                });

                if flow.pending.is_some() {
                    let _ = grant.send(false);
                    return;
                }

                flow.pending = Some(PendingRequest { grant });

                if !flow.queued {
                    flow.queued = true;
                    self.ready.push_back(flow_id);
                }
            }
            Event::Unregister { flow_id } => {
                if let Some(flow) = self.flows.remove(&flow_id) {
                    if let Some(pending) = flow.pending {
                        let _ = pending.grant.send(false);
                    }
                }
            }
        }
    }

    fn grant_next(&mut self) -> bool {
        while let Some(flow_id) = self.ready.pop_front() {
            let Some(flow) = self.flows.get_mut(&flow_id) else {
                continue;
            };

            flow.queued = false;

            let Some(pending) = flow.pending.take() else {
                continue;
            };

            if pending.grant.send(true).is_ok() {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::{Duration, timeout};

    use super::*;

    #[tokio::test]
    async fn grants_are_round_robin_when_two_flows_are_pending() {
        let cancel = CancellationToken::new();
        let fairness = TcpUploadFairness::new(cancel.clone());
        let first = fairness.register();
        let second = fairness.register();

        let first_acquire = first.acquire(128, &cancel);
        let second_acquire = second.acquire(128, &cancel);

        let (first_granted, second_granted) = tokio::join!(first_acquire, second_acquire);

        assert!(first_granted);
        assert!(second_granted);

        assert!(first.acquire(128, &cancel).await);
        assert!(second.acquire(128, &cancel).await);
    }

    #[tokio::test]
    async fn single_active_flow_receives_all_grants() {
        let cancel = CancellationToken::new();
        let fairness = TcpUploadFairness::new(cancel.clone());
        let flow = fairness.register();

        for _ in 0..8 {
            assert!(flow.acquire(128, &cancel).await);
        }
    }

    #[tokio::test]
    async fn dropped_flow_removes_pending_request() {
        let cancel = CancellationToken::new();
        let fairness = TcpUploadFairness::new(cancel.clone());
        let dropped = fairness.register();
        let active = fairness.register();

        let (grant_tx, grant_rx) = oneshot::channel();
        dropped
            .events_tx
            .send(Event::Request {
                flow_id: dropped.id,
                _bytes: 128,
                grant: grant_tx,
            })
            .await
            .expect("scheduler should accept request");

        drop(grant_rx);
        drop(dropped);

        assert!(
            timeout(Duration::from_secs(1), active.acquire(128, &cancel))
                .await
                .expect("active flow should not be blocked by dropped flow")
        );
    }

    #[tokio::test]
    async fn cancellation_stops_pending_acquires() {
        let cancel = CancellationToken::new();
        let fairness = TcpUploadFairness::new(cancel.clone());
        let flow = fairness.register();

        cancel.cancel();

        assert!(
            !timeout(Duration::from_secs(1), flow.acquire(128, &cancel))
                .await
                .expect("cancelled acquire should complete")
        );
    }
}
