use serde::Serialize;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::stats::AgentStats;

use super::tcp_pipe::{PipeClose, PipeCloseKind, TcpPipeCloseHandle};
use super::tcp_pipe::{PipeDirection, TcpPipe};

pub struct TcpClient {
    tunn_to_origin: TcpPipe,
    origin_to_tunn: TcpPipe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClosedBy {
    LocalServer,
    ConnectingClient,
    AgentShutdown,
}

impl std::fmt::Display for ClosedBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalServer => f.write_str("the local server"),
            Self::ConnectingClient => f.write_str("the connecting client"),
            Self::AgentShutdown => f.write_str("agent shutdown"),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TcpClientClose {
    pub(crate) closed_by: ClosedBy,
    pub(crate) initiating_close: PipeClose,
}

#[derive(Clone)]
pub(crate) struct TcpClientCloseHandle {
    tunnel_to_origin: TcpPipeCloseHandle,
    origin_to_tunnel: TcpPipeCloseHandle,
}

impl TcpClient {
    pub async fn create(tunn: TcpStream, origin: TcpStream) -> Self {
        Self::create_with_stats(tunn, origin, None).await
    }

    pub async fn create_with_stats(
        tunn: TcpStream,
        origin: TcpStream,
        stats: Option<AgentStats>,
    ) -> Self {
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

    pub fn is_closed(&self) -> bool {
        self.tunn_to_origin.is_closed() && self.origin_to_tunn.is_closed()
    }

    pub(crate) fn close_handle(&self) -> TcpClientCloseHandle {
        TcpClientCloseHandle {
            tunnel_to_origin: self.tunn_to_origin.close_handle(),
            origin_to_tunnel: self.origin_to_tunn.close_handle(),
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

impl TcpClientCloseHandle {
    pub(crate) async fn wait(self) -> TcpClientClose {
        let (tunnel_to_origin, origin_to_tunnel) =
            tokio::join!(self.tunnel_to_origin.wait(), self.origin_to_tunnel.wait());

        let initiating_close = match (
            tunnel_to_origin.kind == PipeCloseKind::Cancelled,
            origin_to_tunnel.kind == PipeCloseKind::Cancelled,
        ) {
            (true, false) => origin_to_tunnel,
            (false, true) => tunnel_to_origin,
            _ if tunnel_to_origin.finished_at <= origin_to_tunnel.finished_at => tunnel_to_origin,
            _ => origin_to_tunnel,
        };
        let closed_by = closed_by(&initiating_close);

        TcpClientClose {
            closed_by,
            initiating_close,
        }
    }
}

fn closed_by(close: &PipeClose) -> ClosedBy {
    if close.kind == PipeCloseKind::Cancelled {
        return ClosedBy::AgentShutdown;
    }

    match (close.direction, close.kind) {
        (PipeDirection::TunnelToOrigin, PipeCloseKind::Eof | PipeCloseKind::ReadError)
        | (PipeDirection::OriginToTunnel, PipeCloseKind::WriteError) => ClosedBy::ConnectingClient,
        (PipeDirection::OriginToTunnel, PipeCloseKind::Eof | PipeCloseKind::ReadError)
        | (PipeDirection::TunnelToOrigin, PipeCloseKind::WriteError) => ClosedBy::LocalServer,
        (_, PipeCloseKind::Cancelled) => ClosedBy::AgentShutdown,
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{ClosedBy, closed_by};
    use crate::network::tcp::tcp_pipe::{PipeClose, PipeCloseKind, PipeDirection};

    fn close(direction: PipeDirection, kind: PipeCloseKind) -> PipeClose {
        PipeClose {
            direction,
            kind,
            error_kind: Some(io::ErrorKind::ConnectionReset),
            error: Some("test".to_string()),
            finished_at: 1,
        }
    }

    #[test]
    fn attributes_read_closes_to_the_reading_side() {
        assert_eq!(
            closed_by(&close(
                PipeDirection::TunnelToOrigin,
                PipeCloseKind::ReadError
            )),
            ClosedBy::ConnectingClient
        );
        assert_eq!(
            closed_by(&close(PipeDirection::OriginToTunnel, PipeCloseKind::Eof)),
            ClosedBy::LocalServer
        );
    }

    #[test]
    fn attributes_write_closes_to_the_destination_side() {
        assert_eq!(
            closed_by(&close(
                PipeDirection::TunnelToOrigin,
                PipeCloseKind::WriteError
            )),
            ClosedBy::LocalServer
        );
        assert_eq!(
            closed_by(&close(
                PipeDirection::OriginToTunnel,
                PipeCloseKind::WriteError
            )),
            ClosedBy::ConnectingClient
        );
    }
}
