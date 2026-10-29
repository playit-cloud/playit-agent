use std::{
    io::Cursor,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4},
    num::NonZeroU64,
    sync::Arc,
    time::Duration,
};

use playit_agent_core::{
    network::{
        origin_lookup::{OriginIp, OriginLookup, OriginResource, OriginTarget},
        proxy_protocol::ProxyProtocolHeader,
        udp::{
            packets::{PACKET_LEN, Packets},
            udp_clients::UdpClients,
            udp_settings::UdpSettings,
        },
    },
    stats::AgentStats,
};
use playit_agent_proto::{
    PortProto,
    udp_proto::{UdpFlow, UdpFlowExtension},
};
use playit_api_client::api::ProxyProtocol;
use tokio::{net::UdpSocket, time::timeout};

#[tokio::test]
async fn every_udp_datagram_contains_its_proxy_v2_header_and_payload() {
    assert_udp_proxy_framing(ProxyProtocol::ProxyProtocolV2Pkt).await;
}

#[tokio::test]
async fn legacy_v2_sends_one_separate_header_before_unmodified_datagrams() {
    assert_udp_proxy_framing(ProxyProtocol::ProxyProtocolV2).await;
}

async fn assert_udp_proxy_framing(protocol: ProxyProtocol) {
    let origin = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let lookup = Arc::new(OriginLookup::default());
    lookup
        .update(std::iter::once(OriginResource {
            tunnel_id: 42,
            proto: PortProto::Udp,
            target: OriginTarget::Port {
                ip: OriginIp::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port: origin.local_addr().unwrap().port(),
            },
            port_count: 0,
            proxy_protocol: Some(protocol),
        }))
        .await;
    let packets = Packets::new(64);
    let mut clients = UdpClients::new(
        UdpSettings::default(),
        lookup,
        packets.clone(),
        AgentStats::new(),
    );
    let extension = Some(UdpFlowExtension {
        client_server_id: NonZeroU64::new(7).unwrap(),
        tunnel_id: NonZeroU64::new(42).unwrap(),
        port_offset: 0,
    });
    let flows = [
        UdpFlow::V4 {
            src: SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 10), 41000),
            dst: SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 7), 46230),
            frag: None,
            extension,
        },
        UdpFlow::V6 {
            src: ("2001:db8::10".parse::<Ipv6Addr>().unwrap(), 41001),
            dst: ("2001:db8::7".parse::<Ipv6Addr>().unwrap(), 46230),
            extension,
        },
    ];
    let payloads: [&[u8]; 4] = [
        b"first connectivity check",
        &[0x17; PACKET_LEN],
        b"short",
        b"",
    ];
    let mut received = [0; 4096];
    for flow in flows {
        let mut peer: Option<SocketAddr> = None;
        for (index, payload) in payloads.into_iter().enumerate() {
            let mut packet = packets.allocate_wait().await;
            packet.full_slice_mut()[..payload.len()].copy_from_slice(payload);
            packet.set_len(payload.len()).unwrap();
            clients
                .handle_tunneled_packet(1000 + index as u64, flow, packet)
                .await;
            let (mut length, source) =
                timeout(Duration::from_secs(3), origin.recv_from(&mut received))
                    .await
                    .unwrap()
                    .unwrap();
            if let Some(peer) = peer {
                assert_eq!(source, peer);
            }
            peer = Some(source);
            if protocol == ProxyProtocol::ProxyProtocolV2Pkt || index == 0 {
                let mut cursor = Cursor::new(&received[..length]);
                let header = ProxyProtocolHeader::parse_v2_udp(&mut cursor).unwrap();
                assert_eq!(header, ProxyProtocolHeader::from_udp_flow(&flow));
                let header_len = cursor.position() as usize;
                if protocol == ProxyProtocol::ProxyProtocolV2Pkt {
                    assert_eq!(&received[header_len..length], payload);
                    continue;
                }
                assert_eq!(header_len, length);
                let (payload_len, payload_source) =
                    timeout(Duration::from_secs(3), origin.recv_from(&mut received))
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(payload_source, source);
                length = payload_len;
            }
            assert_eq!(&received[..length], payload);
        }
        origin.send_to(b"reply", peer.unwrap()).await.unwrap();
        let reply = timeout(Duration::from_secs(3), clients.recv_origin_packet())
            .await
            .unwrap();
        let (reply_flow, reply_packet) = clients.dispatch_origin_packet(2000, reply).await.unwrap();
        assert_eq!(reply_flow, flow.flip());
        assert_eq!(reply_packet.as_ref(), b"reply");
    }
}
