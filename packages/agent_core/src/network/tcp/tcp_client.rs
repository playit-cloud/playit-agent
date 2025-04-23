use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use super::tcp_pipe::TcpPipe;

pub struct TcpClient {
    tunn_to_origin: TcpPipe,
    origin_to_tunn: TcpPipe,
}

impl TcpClient {
    pub async fn create(tunn: TcpStream, origin: TcpStream) -> Self {
        let (tunn_read, tunn_write) = tunn.into_split();
        let (origin_read, origin_write) = origin.into_split();

        let cancel = CancellationToken::new();

        TcpClient {
            tunn_to_origin: TcpPipe::new_with_cancel(cancel.clone(), tunn_read, origin_write),
            origin_to_tunn: TcpPipe::new_with_cancel(cancel, origin_read, tunn_write),
        }
    }

    pub fn last_use(&self) -> TcpClientLastUse {
        TcpClientLastUse {
            tunn_to_origin: self.tunn_to_origin.last_activity(),
            origin_to_tunn: self.origin_to_tunn.last_activity(),
        }
    }
}

pub struct TcpClientLastUse {
    pub tunn_to_origin: u64,
    pub origin_to_tunn: u64,
}
