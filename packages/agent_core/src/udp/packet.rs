use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Largest datagram (payload plus flow footer) the agent handles.
pub const PACKET_CAPACITY: usize = 2048;

/// Bounded pool of fixed-size datagram buffers.
///
/// The bound is what provides back-pressure: receivers wait in [`PacketPool::allocate`]
/// when every buffer is in flight instead of growing without limit. Buffers are
/// allocated lazily and reused after the [`Packet`] holding them is dropped.
#[derive(Clone)]
pub struct PacketPool {
    free: Arc<Mutex<Vec<Box<[u8]>>>>,
    permits: Arc<Semaphore>,
    capacity: usize,
}

pub struct Packet {
    buffer: Box<[u8]>,
    len: usize,
    free: Arc<Mutex<Vec<Box<[u8]>>>>,
    _permit: OwnedSemaphorePermit,
}

impl PacketPool {
    pub fn new(max_packets: usize) -> Self {
        let max_packets = max_packets.max(1);
        PacketPool {
            free: Arc::new(Mutex::new(Vec::with_capacity(max_packets.min(1024)))),
            permits: Arc::new(Semaphore::new(max_packets)),
            capacity: max_packets,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }

    /// Waits for a buffer. The packet starts with length zero.
    pub async fn allocate(&self) -> Packet {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .expect("packet pool semaphore is never closed");
        self.build(permit)
    }

    pub fn try_allocate(&self) -> Option<Packet> {
        let permit = self.permits.clone().try_acquire_owned().ok()?;
        Some(self.build(permit))
    }

    fn build(&self, permit: OwnedSemaphorePermit) -> Packet {
        let buffer = self
            .free
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .unwrap_or_else(|| vec![0u8; PACKET_CAPACITY].into_boxed_slice());

        Packet {
            buffer,
            len: 0,
            free: self.free.clone(),
            _permit: permit,
        }
    }
}

impl Packet {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn capacity(&self) -> usize {
        PACKET_CAPACITY
    }

    /// The whole buffer, for receiving into. Call [`Packet::set_len`] afterwards.
    pub fn capacity_mut(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    /// Panics if `len` exceeds the capacity; callers only pass lengths of data they
    /// wrote into the buffer.
    pub fn set_len(&mut self, len: usize) {
        assert!(len <= PACKET_CAPACITY, "packet len {len} exceeds capacity");
        self.len = len;
    }

    pub fn truncate(&mut self, len: usize) {
        self.len = self.len.min(len);
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        let buffer = std::mem::take(&mut self.buffer);
        if buffer.len() == PACKET_CAPACITY {
            self.free
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(buffer);
        }
    }
}

impl Deref for Packet {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.buffer[..self.len]
    }
}

impl DerefMut for Packet {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.buffer[..self.len]
    }
}

impl AsRef<[u8]> for Packet {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pool_bounds_outstanding_packets_and_recycles_buffers() {
        let pool = PacketPool::new(2);
        let a = pool.allocate().await;
        let b = pool.allocate().await;
        assert!(pool.try_allocate().is_none());
        assert_eq!(pool.available(), 0);

        drop(a);
        let c = pool.try_allocate().expect("buffer returned to pool");
        assert_eq!(c.len(), 0);
        assert_eq!(pool.available(), 0);

        drop(b);
        drop(c);
        assert_eq!(pool.available(), 2);
        assert_eq!(pool.free.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn waiting_allocation_wakes_when_a_packet_drops() {
        let pool = PacketPool::new(1);
        let held = pool.allocate().await;

        let waiter = {
            let pool = pool.clone();
            tokio::spawn(async move { pool.allocate().await.len() })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        drop(held);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
                .await
                .expect("waiter completes")
                .unwrap(),
            0
        );
    }

    #[test]
    fn packet_views_follow_len() {
        let pool = PacketPool::new(1);
        let mut packet = pool.try_allocate().unwrap();
        packet.capacity_mut()[..3].copy_from_slice(b"abc");
        packet.set_len(3);
        assert_eq!(&*packet, b"abc");
        packet.truncate(2);
        assert_eq!(&*packet, b"ab");
        packet.truncate(10);
        assert_eq!(packet.len(), 2);
    }
}
