use serde::Serialize;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::stats::AgentStats;

use super::tcp_pipe::{PipeDirection, TcpPipe};

pub struct TcpClient {
    tunn_to_origin: TcpPipe,
    origin_to_tunn: TcpPipe,
}

impl TcpClient {
    pub async fn create(tunn: TcpStream, origin: TcpStream) -> Self {
        Self::create_with_stats(tunn, origin, None).await
    }

    pub async fn create_with_stats(tunn: TcpStream, origin: TcpStream, stats: Option<AgentStats>) -> Self {
        let (tunn_read, tunn_write) = tunn.into_split();
        let (origin_read, origin_write) = origin.into_split();

        let cancel = CancellationToken::new();

        TcpClient {
            tunn_to_origin: TcpPipe::new_with_stats(
                cancel.clone(),
                tunn_read,
                origin_write,
                stats.clone(),
                PipeDirection::TunnelToOrigin,
            ),
            origin_to_tunn: TcpPipe::new_with_stats(
                cancel,
                origin_read,
                tunn_write,
                stats,
                PipeDirection::OriginToTunnel,
            ),
        }
    }

    pub fn last_use(&self) -> TcpClientStat {
        TcpClientStat {
            tunn_to_origin: self.tunn_to_origin.last_activity(),
            origin_to_tunn: self.origin_to_tunn.last_activity(),
        }
    }

    pub fn bytes_written(&self) -> TcpClientStat {
        TcpClientStat {
            tunn_to_origin: self.tunn_to_origin.bytes_written(),
            origin_to_tunn: self.origin_to_tunn.bytes_written(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TcpClientStat {
    pub tunn_to_origin: u64,
    pub origin_to_tunn: u64,
}

impl TcpClientStat {
    pub fn min(&self) -> u64 {
        self.tunn_to_origin.min(self.origin_to_tunn)
    }
}
