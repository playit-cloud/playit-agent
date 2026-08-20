use std::{
    num::NonZeroUsize,
    sync::{Arc, atomic::Ordering},
    task::{Poll, Waker},
};

use crossbeam::queue::ArrayQueue;

pub const PACKET_LEN: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketPoolError {
    CapacityOverflow,
}

#[derive(Clone)]
pub struct Packets {
    inner: Arc<PacketsInner>,
}

struct PacketsInner {
    _buffer: Vec<u8>,
    packet_count: usize,
    free_packets: ArrayQueue<*mut u8>,
    waiting: ArrayQueue<Waker>,
}

unsafe impl Send for PacketsInner {}
unsafe impl Sync for PacketsInner {}

pub struct Packet {
    ptr: *mut u8,
    len: usize,
    inner: Arc<PacketsInner>,
}

unsafe impl Send for Packet {}
unsafe impl Sync for Packet {}

impl Packets {
    pub fn new(packet_count: usize) -> Self {
        Self::try_new(NonZeroUsize::new(packet_count).unwrap_or(NonZeroUsize::MIN)).unwrap_or_else(
            |_| Self::try_new(NonZeroUsize::MIN).expect("one packet fits in memory"),
        )
    }

    pub fn try_new(packet_count: NonZeroUsize) -> Result<Self, PacketPoolError> {
        let packet_count = packet_count
            .get()
            .checked_next_power_of_two()
            .ok_or(PacketPoolError::CapacityOverflow)?;
        let bytes = packet_count
            .checked_mul(PACKET_LEN)
            .ok_or(PacketPoolError::CapacityOverflow)?;

        let mut buffer = vec![0u8; bytes];

        let free_packets = ArrayQueue::new(packet_count);
        let ptr = buffer.as_mut_ptr();

        for i in 0..packet_count {
            if free_packets
                .push(unsafe { ptr.add(i * PACKET_LEN) })
                .is_err()
            {
                return Err(PacketPoolError::CapacityOverflow);
            }
        }

        Ok(Packets {
            inner: Arc::new(PacketsInner {
                _buffer: buffer,
                packet_count,
                free_packets,
                waiting: ArrayQueue::new(1024),
            }),
        })
    }

    pub fn packet_count(&self) -> usize {
        self.inner.packet_count
    }

    pub fn allocate(&self) -> Option<Packet> {
        let ptr = self.inner.free_packets.pop()?;
        Some(Packet {
            ptr,
            len: PACKET_LEN,
            inner: self.inner.clone(),
        })
    }

    pub async fn allocate_wait(&self) -> Packet {
        std::future::poll_fn(|cx| {
            std::sync::atomic::fence(Ordering::Acquire);

            if let Some(ptr) = self.inner.free_packets.pop() {
                return Poll::Ready(Packet {
                    ptr,
                    len: PACKET_LEN,
                    inner: self.inner.clone(),
                });
            }

            if let Err(waker) = self.inner.waiting.push(cx.waker().clone()) {
                // Queue full - schedule a retry after yielding
                waker.wake();
            }

            Poll::Pending
        })
        .await
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        if self.inner.free_packets.push(self.ptr).is_err() {
            tracing::error!("packet pool rejected a returned buffer");
            return;
        }

        if let Some(wake) = self.inner.waiting.pop() {
            std::sync::atomic::fence(Ordering::Release);
            wake.wake();
        }
    }
}

impl AsMut<[u8]> for Packet {
    fn as_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl AsRef<[u8]> for Packet {
    fn as_ref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Packet {
    pub fn full_slice_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, PACKET_LEN) }
    }

    pub fn full_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, PACKET_LEN) }
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn set_len(&mut self, len: usize) -> std::io::Result<()> {
        if PACKET_LEN < len {
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

    #[test]
    fn pool_rejects_overload_and_reuses_returned_packets() {
        let packets = Packets::new(3);
        assert_eq!(packets.packet_count(), 4);
        let mut allocated = Vec::new();
        for _ in 0..4 {
            allocated.push(packets.allocate().expect("packet available"));
        }
        assert!(packets.allocate().is_none());
        drop(allocated.pop());
        assert!(packets.allocate().is_some());
    }

    #[test]
    fn sustained_reuse_never_grows_the_pool() {
        let packets = Packets::new(8);
        for _ in 0..10_000 {
            let packet = packets.allocate().expect("packet available");
            drop(packet);
        }
        assert_eq!(packets.packet_count(), 8);
    }
}
