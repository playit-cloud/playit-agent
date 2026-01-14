use std::{
    sync::{atomic::Ordering, Arc},
    task::{Poll, Waker},
};

use crossbeam::queue::ArrayQueue;

pub const PACKET_LEN: usize = 2048;

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
    pub fn new(mut packet_count: usize) -> Self {
        packet_count = packet_count.next_power_of_two();
        let bytes = packet_count * PACKET_LEN;

        let mut buffer = vec![0u8; bytes];

        let free_packets = ArrayQueue::new(packet_count);
        let ptr = buffer.as_mut_ptr();

        for i in 0..packet_count {
            free_packets
                .push(unsafe { ptr.add(i * PACKET_LEN) })
                .expect("free packet queue too small");
        }

        Packets {
            inner: Arc::new(PacketsInner {
                _buffer: buffer,
                packet_count,
                free_packets,
                waiting: ArrayQueue::new(1024),
            }),
        }
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
        self.inner
            .free_packets
            .push(self.ptr)
            .expect("free packet queue full");

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
