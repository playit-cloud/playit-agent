use std::sync::Arc;

use crossbeam::queue::ArrayQueue;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const PACKET_LEN: usize = 2048;

#[derive(Clone)]
pub struct Packets {
    inner: Arc<PacketsInner>,
}

struct PacketsInner {
    free: ArrayQueue<Box<[u8; PACKET_LEN]>>,
    available: Arc<Semaphore>,
}

pub struct Packet {
    buffer: Option<Box<[u8; PACKET_LEN]>>,
    len: usize,
    inner: Arc<PacketsInner>,
    // Return the buffer before releasing its permit.
    _permit: OwnedSemaphorePermit,
}

impl Packets {
    pub fn new(packet_count: usize) -> Self {
        let packet_count = packet_count.max(1);
        let free = ArrayQueue::new(packet_count);
        for _ in 0..packet_count {
            free.push(Box::new([0; PACKET_LEN])).unwrap();
        }
        Self {
            inner: Arc::new(PacketsInner {
                free,
                available: Arc::new(Semaphore::new(packet_count)),
            }),
        }
    }

    pub fn packet_count(&self) -> usize {
        self.inner.free.capacity()
    }

    fn with_permit(&self, permit: OwnedSemaphorePermit) -> Packet {
        Packet {
            buffer: Some(self.inner.free.pop().expect("permit reserves a buffer")),
            len: PACKET_LEN,
            inner: self.inner.clone(),
            _permit: permit,
        }
    }

    pub fn allocate(&self) -> Option<Packet> {
        let permit = self.inner.available.clone().try_acquire_owned().ok()?;
        Some(self.with_permit(permit))
    }

    pub async fn allocate_wait(&self) -> Packet {
        let permit = self
            .inner
            .available
            .clone()
            .acquire_owned()
            .await
            .expect("packet pool semaphore is never closed");
        self.with_permit(permit)
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        self.inner.free.push(self.buffer.take().unwrap()).unwrap();
    }
}

impl AsMut<[u8]> for Packet {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.buffer.as_mut().unwrap()[..self.len]
    }
}

impl AsRef<[u8]> for Packet {
    fn as_ref(&self) -> &[u8] {
        &self.buffer.as_ref().unwrap()[..self.len]
    }
}

impl Packet {
    pub fn full_slice_mut(&mut self) -> &mut [u8] {
        self.buffer.as_mut().unwrap().as_mut_slice()
    }

    pub fn full_slice(&self) -> &[u8] {
        self.buffer.as_ref().unwrap().as_slice()
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn set_len(&mut self, len: usize) -> std::io::Result<()> {
        if len > PACKET_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "packet len too large",
            ));
        }
        self.len = len;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelled_waiter_does_not_consume_returned_buffer() {
        let pool = Packets::new(1);
        let packet = pool.allocate().unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), pool.allocate_wait())
                .await
                .is_err()
        );
        let waiter = tokio::spawn({
            let pool = pool.clone();
            async move { pool.allocate_wait().await }
        });
        tokio::task::yield_now().await;
        drop(packet);
        let packet = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        assert!(pool.allocate().is_none());
        drop(packet);
        assert!(pool.allocate().is_some());
    }
}
