use serde::Serialize;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::stats::AgentStats;

use super::tcp_pipe::{PipeDirection, TcpPipe};

pub struct TcpClient {
    tunn_to_origin: TcpPipe,
    origin_to_tunn: TcpPipe,
}

impl TcpClient {
    pub async fn spawn(tunn: TcpStream, origin: TcpStream) -> Self {
        Self::spawn_with_stats(tunn, origin, None).await
    }

    pub async fn spawn_with_stats(
        tunn: TcpStream,
        origin: TcpStream,
        stats: Option<AgentStats>,
    ) -> Self {
        let (tunn_read, tunn_write) = tunn.into_split();
        let (origin_read, origin_write) = origin.into_split();

        let cancel = CancellationToken::new();

        TcpClient {
            tunn_to_origin: TcpPipe::spawn_with_stats(
                cancel.clone(),
                tunn_read,
                origin_write,
                stats.clone(),
                PipeDirection::TunnelToOrigin,
            ),
            origin_to_tunn: TcpPipe::spawn_with_stats(
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

    pub fn failure(&self) -> Option<crate::playit_agent::ServiceExit> {
        [self.tunn_to_origin.exit(), self.origin_to_tunn.exit()]
            .into_iter()
            .flatten()
            .find(|exit| {
                !matches!(
                    exit,
                    crate::playit_agent::ServiceExit::Cancelled
                        | crate::playit_agent::ServiceExit::CleanEof
                )
            })
    }

    pub async fn shutdown(self, deadline: Duration) -> crate::playit_agent::ServiceExit {
        let (left, right) = tokio::join!(
            self.tunn_to_origin.join(deadline),
            self.origin_to_tunn.join(deadline)
        );
        if matches!(left, crate::playit_agent::ServiceExit::DeadlineExceeded)
            || matches!(right, crate::playit_agent::ServiceExit::DeadlineExceeded)
        {
            crate::playit_agent::ServiceExit::DeadlineExceeded
        } else if let Some(exit) = [left, right].into_iter().find(|exit| {
            !matches!(
                exit,
                crate::playit_agent::ServiceExit::Cancelled
                    | crate::playit_agent::ServiceExit::Completed
                    | crate::playit_agent::ServiceExit::CleanEof
            )
        }) {
            exit
        } else {
            crate::playit_agent::ServiceExit::Cancelled
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
