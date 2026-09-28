use playit_agent_proto::control_feed::NewClient;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::network::tcp::tcp_errors::tcp_errors;
use crate::stats::AgentStats;

use super::{JoinContext, JoinError, proxy_http_with_timeout, sessions::NetherNetSessions};

pub(crate) struct NetherNetTcp;

impl NetherNetTcp {
    pub async fn handle(
        tunnel: TcpStream,
        origin: TcpStream,
        details: &NewClient,
        client_id: u64,
        sessions: &NetherNetSessions,
        stats: &AgentStats,
        cancel: &CancellationToken,
    ) {
        let context = JoinContext {
            tunnel_id: details.tunnel_id,
            connect_addr: details.connect_addr,
            peer_addr: details.peer_addr,
        };
        let joined = tokio::select! {
            _ = cancel.cancelled() => return,
            res = proxy_http_with_timeout(tunnel, origin, context, sessions, stats) => res,
        };
        match joined {
            Ok(outcome) => {
                tracing::debug!(?outcome, id = client_id, "NetherNet join proxied");
            }
            Err(error) => {
                tracing::warn!(
                    ?error,
                    id = client_id,
                    tunnel_id = details.tunnel_id,
                    "NetherNet join failed"
                );
                let errors = tcp_errors();
                match error {
                    JoinError::Request(_) => errors.nethernet_join_request_error.inc(),
                    JoinError::Origin(_) => errors.nethernet_join_origin_error.inc(),
                    JoinError::UnexpectedReply | JoinError::Answer(_) | JoinError::TooManyJoins => {
                        errors.nethernet_join_answer_error.inc()
                    }
                    JoinError::WriteTunnel(_) => {}
                    JoinError::Timeout => errors.nethernet_join_timeout.inc(),
                }
            }
        }
    }
}
