use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use crate::network::tcp_clients::TcpClient;
use crate::utils::now_milli;

pub async fn pipe<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut from: R,
    mut to: W,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    buffer.resize(2048, 0u8);

    loop {
        tokio::task::yield_now().await;

        let received = match tokio::time::timeout(Duration::from_secs(200), from.read(&mut buffer[..])).await {
            Ok(Ok(received)) => {
                received
            }
            Ok(Err(error)) => {
                tracing::error!(?error, "failed to read data");
                return Err(error);
            }
            Err(_) => break,
        };

        if received == 0 {
            tracing::info!("pipe ended due to EOF");
            break;
        }

        to.write_all(&buffer[..received]).await.map_err(|error| {
            tracing::error!(?error, "failed to write data");
            error
        })?;
    }

    Ok(())
}
