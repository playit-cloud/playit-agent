use circular_buffer::CircularBuffer;
use tokio::sync::Mutex;

pub struct LoggingBuffer {
    buffer: Mutex<Box<CircularBuffer<131072 /* 128KB */, u8>>>,
}

impl LoggingBuffer {
    pub fn foo(&self) {}
}
