use std::net::SocketAddr;

use playit_agent_proto::control_feed::ClaimInstructions;
use playit_api_client::api::ProxyProtocol;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use crate::origin::{lan, proxy_protocol::ProxyProtocolHeader};

/// Length of the acknowledgement the tunnel server writes after accepting a claim token.
const CLAIM_ACK_LEN: usize = 8;

/// Connects to the tunnel server and claims the waiting client with its token.
pub async fn claim(claim: &ClaimInstructions, no_delay: bool) -> std::io::Result<TcpStream> {
    let mut stream = TcpStream::connect(claim.address).await?;
    if let Err(error) = stream.set_nodelay(no_delay) {
        tracing::warn!(
            ?error,
            no_delay,
            "failed to set TCP_NODELAY on tunnel stream"
        );
    }

    stream.write_all(&claim.token).await?;

    let mut ack = [0u8; CLAIM_ACK_LEN];
    stream.read_exact(&mut ack).await?;
    Ok(stream)
}

#[derive(Debug)]
pub enum OriginError {
    Connect(std::io::Error),
    ProxyHeader(std::io::Error),
}

/// Connects to the local origin on behalf of `peer` and writes the proxy protocol
/// header if the tunnel is configured for one.
pub async fn origin(
    client_loopback_ip: bool,
    peer: SocketAddr,
    origin_addr: SocketAddr,
    proxy: Option<(ProxyProtocol, ProxyProtocolHeader)>,
) -> Result<TcpStream, OriginError> {
    let mut stream = lan::connect_tcp(client_loopback_ip, peer, origin_addr)
        .await
        .map_err(OriginError::Connect)?;

    if let Err(error) = stream.set_nodelay(true) {
        tracing::warn!(?error, "failed to set TCP_NODELAY on origin stream");
    }

    match proxy {
        Some((ProxyProtocol::ProxyProtocolV1, header)) => header
            .write_v1_tcp(&mut stream)
            .await
            .map_err(OriginError::ProxyHeader)?,
        Some((ProxyProtocol::ProxyProtocolV2, header)) => header
            .write_v2_tcp(&mut stream)
            .await
            .map_err(OriginError::ProxyHeader)?,
        None => {}
    }

    Ok(stream)
}
