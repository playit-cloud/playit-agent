use super::tcp_errors::tcp_errors;
use crate::network::{
    errors::IntCounter, lan_address::LanAddress, origin_lookup::OriginLookup,
    proxy_protocol::ProxyProtocolHeader,
};
use playit_agent_proto::control_feed::NewClient;
use playit_api_client::api::ProxyProtocol;
use std::{future::Future, io, net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub(super) struct ConnectedClient {
    pub tunnel: TcpStream,
    pub origin: TcpStream,
    pub origin_addr: SocketAddr,
}

async fn step<T>(
    operation: impl Future<Output = io::Result<T>>,
    seconds: u64,
    io_errors: &IntCounter,
    timeouts: &IntCounter,
) -> io::Result<T> {
    match tokio::time::timeout(Duration::from_secs(seconds), operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            io_errors.inc();
            Err(error)
        }
        Err(_) => {
            timeouts.inc();
            Err(io::ErrorKind::TimedOut.into())
        }
    }
}

pub(super) async fn connect(
    details: &NewClient,
    lookup: &OriginLookup,
    no_delay: bool,
) -> io::Result<ConnectedClient> {
    let errors = tcp_errors();
    let found = lookup
        .lookup(details.tunnel_id, true)
        .await
        .ok_or_else(|| {
            errors.new_client_origin_not_found.inc();
            io::Error::new(io::ErrorKind::NotFound, "TCP origin missing")
        })?;
    let origin_addr = found
        .resolve_local(details.port_offset)
        .await
        .ok_or_else(|| {
            errors.new_client_invalid_port_offset.inc();
            io::Error::new(io::ErrorKind::InvalidInput, "TCP origin resolution failed")
        })?;
    let header = match (details.peer_addr, details.connect_addr) {
        (SocketAddr::V4(peer), SocketAddr::V4(tunnel)) => ProxyProtocolHeader::AfInet {
            client_ip: *peer.ip(),
            proxy_ip: *tunnel.ip(),
            client_port: peer.port(),
            proxy_port: tunnel.port(),
        },
        (SocketAddr::V6(peer), SocketAddr::V6(tunnel)) => ProxyProtocolHeader::AfInet6 {
            client_ip: *peer.ip(),
            proxy_ip: *tunnel.ip(),
            client_port: peer.port(),
            proxy_port: tunnel.port(),
        },
        _ => {
            errors.invalid_proto_match.inc();
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TCP address families differ",
            ));
        }
    };
    let mut tunnel = step(
        TcpStream::connect(details.claim_instructions.address),
        8,
        &errors.new_client_claim_connect_error,
        &errors.new_client_claim_connect_timeout,
    )
    .await?;
    if tunnel.set_nodelay(no_delay).is_err() {
        errors.new_client_set_tunnel_no_delay_error.inc();
    }
    step(
        tunnel.write_all(&details.claim_instructions.token),
        8,
        &errors.new_client_send_claim_error,
        &errors.new_client_send_claim_timeout,
    )
    .await?;
    // The existing claim protocol consumes an opaque eight-byte acknowledgement.
    let mut acknowledgement = [0; 8];
    step(
        tunnel.read_exact(&mut acknowledgement),
        4,
        &errors.new_client_claim_expect_error,
        &errors.new_client_claim_expect_timeout,
    )
    .await?;
    let mut origin = step(
        LanAddress::tcp_socket(true, details.peer_addr, origin_addr),
        2,
        &errors.new_client_origin_connect_error,
        &errors.new_client_origin_connect_timeout,
    )
    .await?;
    if origin.set_nodelay(no_delay).is_err() {
        errors.new_client_set_origin_no_delay_error.inc();
    }
    step(
        async {
            match found.proxy_protocol {
                Some(ProxyProtocol::ProxyProtocolV1) => header.write_v1_tcp(&mut origin).await,
                Some(ProxyProtocol::ProxyProtocolV2) => header.write_v2_tcp(&mut origin).await,
                None => Ok(()),
            }
        },
        2,
        &errors.new_client_write_proxy_proto_error,
        &errors.new_client_write_proxy_proto_timeout,
    )
    .await?;
    Ok(ConnectedClient {
        tunnel,
        origin,
        origin_addr,
    })
}
