use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use playit_agent_core::network::origin_lookup::{
    OriginIp, OriginLookup, OriginResource, OriginTarget,
};
use playit_agent_core::network::tcp::tcp_clients::TcpClients;
use playit_agent_core::network::tcp::tcp_settings::TcpSettings;
use playit_agent_core::stats::AgentStats;
use playit_agent_proto::PortProto;
use playit_agent_proto::control_feed::{ClaimInstructions, NewClient};
use playit_api_client::api::ProxyProtocol;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);
const CLAIM_TOKEN: &[u8] = b"integration-claim-token";

#[tokio::test]
async fn tcp_tunnel_relays_in_both_directions_without_proxy_protocol() {
    exercise_tcp_tunnel(None).await;
}

#[tokio::test]
async fn tcp_tunnel_writes_proxy_protocol_v1_before_origin_payload() {
    exercise_tcp_tunnel(Some(ProxyProtocol::ProxyProtocolV1)).await;
}

async fn exercise_tcp_tunnel(proxy_protocol: Option<ProxyProtocol>) {
    let expect_proxy_protocol = proxy_protocol.is_some();
    let claim_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind claim listener");
    let origin_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind origin listener");
    let claim_addr = claim_listener.local_addr().expect("claim listener address");
    let origin_addr = origin_listener
        .local_addr()
        .expect("origin listener address");

    let lookup = Arc::new(OriginLookup::default());
    lookup
        .update(std::iter::once(OriginResource {
            tunnel_id: 42,
            proto: PortProto::Tcp,
            target: OriginTarget::Port {
                ip: OriginIp::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port: origin_addr.port(),
            },
            port_count: 0,
            proxy_protocol,
        }))
        .await;

    let stats = AgentStats::new();
    let cancel = CancellationToken::new();
    let clients = TcpClients::new(
        TcpSettings::default(),
        lookup,
        stats.clone(),
        cancel.child_token(),
    );
    let peer_addr: SocketAddr = "198.51.100.10:40123".parse().expect("peer address");
    let connect_addr: SocketAddr = "203.0.113.20:5525".parse().expect("connect address");

    clients
        .handle_new_client(NewClient {
            connect_addr,
            peer_addr,
            data_center_id: 1,
            tunnel_id: 42,
            port_offset: 0,
            claim_instructions: ClaimInstructions {
                address: claim_addr,
                token: CLAIM_TOKEN.to_vec(),
            },
        })
        .await;

    let (mut tunnel_stream, _) = timeout(TEST_TIMEOUT, claim_listener.accept())
        .await
        .expect("claim connection timeout")
        .expect("accept claim connection");
    let mut received_token = vec![0; CLAIM_TOKEN.len()];
    tunnel_stream
        .read_exact(&mut received_token)
        .await
        .expect("read claim token");
    assert_eq!(received_token, CLAIM_TOKEN);
    tunnel_stream
        .write_all(b"accepted")
        .await
        .expect("confirm claimed connection");

    let (mut origin_stream, _) = timeout(TEST_TIMEOUT, origin_listener.accept())
        .await
        .expect("origin connection timeout")
        .expect("accept origin connection");

    if expect_proxy_protocol {
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n") {
            let mut byte = [0];
            timeout(TEST_TIMEOUT, origin_stream.read_exact(&mut byte))
                .await
                .expect("proxy header timeout")
                .expect("read proxy header");
            header.push(byte[0]);
        }
        assert_eq!(
            String::from_utf8(header).expect("UTF-8 proxy header"),
            format!("PROXY TCP4 198.51.100.10 203.0.113.20 40123 5525\r\n")
        );
    }

    let inbound = b"payload from tunnel";
    tunnel_stream
        .write_all(inbound)
        .await
        .expect("write tunnel payload");
    let mut received_inbound = vec![0; inbound.len()];
    timeout(
        TEST_TIMEOUT,
        origin_stream.read_exact(&mut received_inbound),
    )
    .await
    .expect("origin payload timeout")
    .expect("read origin payload");
    assert_eq!(received_inbound, inbound);

    let outbound = b"reply from origin";
    origin_stream
        .write_all(outbound)
        .await
        .expect("write origin reply");
    let mut received_outbound = vec![0; outbound.len()];
    timeout(
        TEST_TIMEOUT,
        tunnel_stream.read_exact(&mut received_outbound),
    )
    .await
    .expect("tunnel reply timeout")
    .expect("read tunnel reply");
    assert_eq!(received_outbound, outbound);

    timeout(TEST_TIMEOUT, async {
        loop {
            let details = clients.get_details().await;
            if let Some(client) = details.first()
                && client.bytes_written.tunn_to_origin >= inbound.len() as u64
                && client.bytes_written.origin_to_tunn >= outbound.len() as u64
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("TCP client stats timeout");

    assert_eq!(stats.bytes_in(), inbound.len() as u64);
    assert_eq!(stats.bytes_out(), outbound.len() as u64);
    cancel.cancel();
}
