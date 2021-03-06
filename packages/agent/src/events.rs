use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use tokio::sync::{RwLock, RwLockMappedWriteGuard};

use agent_common::Proto;

const MAX_EVENT_COUNT: usize = 100;
const TARGET_EVENT_COUNT: usize = 50;

#[derive(Clone)]
pub struct PlayitEvents {
    inner: Arc<RwLock<Inner>>,
}

struct Inner {
    items: Vec<PlayitEvent>,
    next_id: usize,
}

impl PlayitEvents {
    pub fn new() -> Self {
        PlayitEvents {
            inner: Arc::new(RwLock::new(Inner {
                items: vec![],
                next_id: 1,
            }))
        }
    }

    pub async fn add_event(&self, details: PlayitEventDetails) {
        let mut guard = self.inner.write().await;

        let id = guard.next_id;
        guard.next_id = id + 1;

        guard.items.push(PlayitEvent {
            id,
            details,
        });

        if guard.items.len() > MAX_EVENT_COUNT {
            guard.items.drain(..TARGET_EVENT_COUNT);
        }
    }

    pub fn with_events<T, F: Fn(&[PlayitEvent]) -> T>(&self, handle: F) -> T {
        let guard = futures::executor::block_on(self.inner.read());
        handle(guard.items.as_slice())
    }
}

pub struct PlayitEvent {
    pub id: usize,
    pub details: PlayitEventDetails,
}

#[derive(Debug)]
pub enum PlayitEventDetails {
    ClientAccepted {
        client_id: u64,
        proto: Proto,
        tunnel_addr: SocketAddr,
        peer_addr: SocketAddr,
        host_addr: SocketAddr,
    },
    NewClientSetupFailed {
        client_id: u64,
        reason: SetupFailReason,
    },
    ClientConnected {
        client_id: u64,
    },
    ClientDisconnected {
        client_id: u64,
    },
    AgentConfigUpdated,
}

#[derive(Debug)]
pub enum SetupFailReason {
    TunnelServerNoConnect(std::io::Error),
    LocalServerNoConnect(std::io::Error),
}