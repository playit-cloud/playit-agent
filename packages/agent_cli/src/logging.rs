use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing_subscriber::fmt::MakeWriter;

pub struct LoggingBuffer {
    sender: tokio::sync::mpsc::Sender<String>,
    reader: Option<tokio::sync::mpsc::Receiver<String>>,
    dropped: Arc<AtomicU64>,
}

impl LoggingBuffer {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(1024);

        LoggingBuffer {
            sender: tx,
            reader: Some(rx),
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn reader(&mut self) -> Option<LogReader> {
        let reader = self.reader.take()?;

        Some(LogReader {
            reader,
            dropped: self.dropped.clone(),
            sent_drop_last: false,
        })
    }
}

pub struct LogReader {
    reader: tokio::sync::mpsc::Receiver<String>,
    dropped: Arc<AtomicU64>,
    sent_drop_last: bool,
}

impl LogReader {
    pub fn try_read(&mut self) -> Option<String> {
        if !self.sent_drop_last {
            let dropped = self.dropped.swap(0, Ordering::SeqCst);

            if dropped > 0 {
                self.sent_drop_last = true;
                return Some(format!("dropped {} log messages", dropped));
            }
        }

        self.sent_drop_last = false;
        self.reader.try_recv().ok()
    }
}

pub struct LogWriter {
    buffer: Vec<u8>,
    sender: tokio::sync::mpsc::Sender<String>,
    messages_dropped: Arc<AtomicU64>,
}

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for i in 0..buf.len() {
            let byte = buf[i];

            if byte == '\n' as u8 && i > 0 {
                self.buffer.extend(&buf[..i]);

                if let Ok(string) = String::from_utf8(std::mem::replace(&mut self.buffer, Vec::new())) {
                    if self.sender.try_send(string).is_err() {
                        self.messages_dropped.fetch_add(1, Ordering::SeqCst);
                    }
                }

                return Ok(i);
            }
        }

        self.buffer.extend(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LoggingBuffer {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter {
            buffer: vec![],
            sender: self.sender.clone(),
            messages_dropped: self.dropped.clone(),
        }
    }
}