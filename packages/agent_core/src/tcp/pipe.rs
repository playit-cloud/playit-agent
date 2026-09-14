use std::time::Duration;

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::{Instant, sleep_until},
};

use super::ConnectionInfo;
use crate::stats::AgentStats;

const BUFFER_LEN: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeOutcome {
    /// Both directions reached end of stream.
    Closed,
    /// One direction failed; the connection was torn down.
    Error,
    /// Nothing moved in either direction for the idle timeout.
    Idle,
}

/// Copies bytes between the tunnel and origin streams until done.
///
/// A half-close on one side (EOF) is forwarded as a write shutdown on the other so
/// protocols relying on it keep working; the other direction keeps flowing. An error in
/// either direction drops both streams.
pub async fn run(
    mut tunnel: TcpStream,
    mut origin: TcpStream,
    info: &ConnectionInfo,
    stats: &AgentStats,
    idle_timeout: Duration,
) -> PipeOutcome {
    info.touch();

    let (mut tunnel_read, mut tunnel_write) = tunnel.split();
    let (mut origin_read, mut origin_write) = origin.split();

    let to_origin = copy(&mut tunnel_read, &mut origin_write, |bytes| {
        info.record_to_origin(bytes);
        stats.add_bytes_in(bytes as u64);
    });
    let to_tunnel = copy(&mut origin_read, &mut tunnel_write, |bytes| {
        info.record_to_tunnel(bytes);
        stats.add_bytes_out(bytes as u64);
    });

    tokio::select! {
        result = async { tokio::try_join!(to_origin, to_tunnel) } => match result {
            Ok(_) => PipeOutcome::Closed,
            Err(error) => {
                tracing::debug!(?error, "tcp pipe failed");
                PipeOutcome::Error
            }
        },
        _ = idle_watchdog(info, idle_timeout) => PipeOutcome::Idle,
    }
}

async fn copy<R, W, F>(from: &mut R, to: &mut W, mut on_bytes: F) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnMut(usize),
{
    let mut buffer = vec![0u8; BUFFER_LEN];

    loop {
        let read = from.read(&mut buffer).await?;
        if read == 0 {
            // Forward the half-close; the peer may already be gone, which is fine.
            let _ = to.shutdown().await;
            return Ok(());
        }

        to.write_all(&buffer[..read]).await?;
        on_bytes(read);
    }
}

async fn idle_watchdog(info: &ConnectionInfo, idle_timeout: Duration) {
    loop {
        let deadline = info.last_activity() + idle_timeout;
        if deadline <= Instant::now() {
            return;
        }
        sleep_until(deadline).await;
    }
}
