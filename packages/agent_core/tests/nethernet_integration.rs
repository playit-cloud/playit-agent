#[cfg(test)]
mod test {
    use playit_agent_core::{
        network::{
            nethernet::{
                self, JoinContext, JoinOutcome, http_message::HttpMessage,
                sessions::NetherNetSessions,
            },
            origin_lookup::{OriginIp, OriginLookup, OriginResource, OriginTarget},
            udp::{
                packets::{Packet, Packets},
                udp_clients::UdpClients,
                udp_settings::UdpSettings,
            },
        },
        stats::AgentStats,
        utils::now_milli,
    };
    use playit_agent_proto::{
        PortProto,
        udp_proto::{UdpFlow, UdpFlowExtension},
    };
    use std::{
        net::{Ipv4Addr, SocketAddr, SocketAddrV4},
        num::NonZeroU64,
        sync::Arc,
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream, UdpSocket},
        time::timeout,
    };

    const WAIT: Duration = Duration::from_secs(3);

    struct Harness {
        sessions: Arc<NetherNetSessions>,
        clients: UdpClients,
        packets: Packets,
    }

    impl Harness {
        async fn new() -> Self {
            let sessions = Arc::new(NetherNetSessions::new());
            let lookup = Arc::new(OriginLookup::default());
            lookup
                .update(std::iter::once(OriginResource {
                    tunnel_id: 42,
                    proto: PortProto::Both,
                    target: OriginTarget::Port {
                        ip: OriginIp::IpAddress(Ipv4Addr::LOCALHOST.into()),
                        port: 19132,
                    },
                    port_count: 1,
                    proxy_protocol: None,
                    nethernet: true,
                }))
                .await;
            let packets = Packets::new(64);
            let clients = UdpClients::new(
                UdpSettings::default(),
                lookup,
                packets.clone(),
                AgentStats::new(),
                sessions.clone(),
            );
            Self {
                sessions,
                clients,
                packets,
            }
        }

        fn packet(&self, data: &[u8]) -> Packet {
            let mut packet = self.packets.allocate().unwrap();
            packet.as_mut()[..data.len()].copy_from_slice(data);
            packet.set_len(data.len()).unwrap();
            packet
        }

        async fn send(&mut self, now: u64, flow: UdpFlow, data: &[u8]) {
            let packet = self.packet(data);
            self.clients.handle_tunneled_packet(now, flow, packet).await;
        }

        async fn reply(
            &mut self,
            now: u64,
            origin: &UdpSocket,
            peer: SocketAddr,
            flow: UdpFlow,
            data: &[u8],
        ) {
            origin.send_to(data, peer).await.unwrap();
            let received = timeout(WAIT, self.clients.recv_origin_packet())
                .await
                .unwrap();
            let (reply_flow, packet) = self
                .clients
                .dispatch_origin_packet(now, received)
                .await
                .unwrap();
            assert_eq!(reply_flow, flow.flip());
            assert_eq!(packet.as_ref(), data);
            // The ordinary footer still encodes and decodes the reverse flow.
            let mut footer = vec![0; reply_flow.footer_len()];
            assert!(reply_flow.write_to(&mut footer));
            assert_eq!(UdpFlow::from_tail(&footer), Ok(reply_flow));
        }

        async fn join(&self, flow: UdpFlow, candidate: SocketAddr, ufrag: &str) {
            let (mut client, tunnel) = tcp_pair().await;
            let (origin, mut server) = tcp_pair().await;
            let offer = "v=0\r\na=identity:untouched\r\na=candidate:1 1 udp 100 198.51.100.10 40000 typ host\r\n";
            let request = format!(
                "POST /v1/join/123 HTTP/1.1\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{offer}",
                offer.len()
            );
            let sessions = self.sessions.clone();
            let task = tokio::spawn(async move {
                nethernet::proxy_http_with_timeout(
                    tunnel,
                    origin,
                    context(flow),
                    &sessions,
                    &AgentStats::new(),
                )
                .await
            });
            client.write_all(request.as_bytes()).await.unwrap();
            let forwarded = HttpMessage::read(&mut server, 1024, 8192).await.unwrap();
            assert_eq!(forwarded.body, b"v=0\r\na=identity:untouched\r\n");
            let answer = format!(
                "v=0\r\no=- 1 1 IN IP4 {}\r\nm=application {} UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 {}\r\na=candidate:1 1 udp 100 {} {} typ host\r\na=ice-ufrag:{ufrag}\r\na=ice-pwd:test-password\r\n",
                candidate.ip(),
                candidate.port(),
                candidate.ip(),
                candidate.ip(),
                candidate.port()
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{answer}",
                answer.len()
            );
            server.write_all(response.as_bytes()).await.unwrap();
            let rewritten = HttpMessage::read(&mut client, 1024, 8192).await.unwrap();
            let body = String::from_utf8(rewritten.body).unwrap();
            assert!(body.contains("203.0.113.7 30123 typ host"));
            assert!(!body.contains(&candidate.ip().to_string()));
            assert_eq!(task.await.unwrap().unwrap(), JoinOutcome::Answer);
        }
    }

    fn flow(port: u16) -> UdpFlow {
        UdpFlow::V4 {
            src: SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 10), port),
            dst: SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 7), 30123),
            frag: None,
            extension: Some(UdpFlowExtension {
                client_server_id: NonZeroU64::new(7).unwrap(),
                tunnel_id: NonZeroU64::new(42).unwrap(),
                port_offset: 0,
            }),
        }
    }

    fn context(flow: UdpFlow) -> JoinContext {
        JoinContext {
            tunnel_id: 42,
            connect_addr: flow.dst(),
            peer_addr: flow.src(),
        }
    }

    fn check(ufrag: &[u8; 4]) -> Vec<u8> {
        let mut bytes = hex::decode("000100502112a4425949507037627963376f434200060009724a62373a53576b2f000000c057000400020000802a0008a7b6e8826927321100250000002400046e7e1eff0008001472cc63fe85b3a06cf6913cc142d35fd928131d9c802800043562cf76").unwrap();
        bytes[24..28].copy_from_slice(ufrag);
        bytes
    }

    fn success() -> Vec<u8> {
        let mut bytes = vec![0; 20];
        bytes[..2].copy_from_slice(&[1, 1]);
        bytes[4..8].copy_from_slice(&[0x21, 0x12, 0xa4, 0x42]);
        bytes
    }

    async fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        (client, listener.accept().await.unwrap().0)
    }

    /// A fake Bedrock server socket. A loopback signaling origin makes the agent send
    /// to this machine's LAN address instead, so listen on every interface.
    async fn origin() -> UdpSocket {
        UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap()
    }

    async fn recv(socket: &UdpSocket, expected: &[u8]) -> SocketAddr {
        let mut data = [0; 2048];
        let (len, peer) = timeout(WAIT, socket.recv_from(&mut data))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&data[..len], expected);
        peer
    }

    #[tokio::test]
    async fn one_public_port_multiplexes_joins_replies_and_reused_client_endpoints() {
        timeout(WAIT, async {
            let mut h = Harness::new().await;
            let a = origin().await;
            let b = origin().await;
            let c = origin().await;
            let fa = flow(41000);
            let fb = flow(41001);
            h.join(fa, a.local_addr().unwrap(), "aaaa").await;
            h.join(fb, b.local_addr().unwrap(), "bbbb").await;
            let now = now_milli();

            // Unknown sessions must not consume the quota needed by these two joins.
            for _ in 0..100 {
                h.send(now, flow(42000), &check(b"xxxx")).await;
            }
            h.send(now, fa, &check(b"aaaa")).await;
            let pa = recv(&a, &check(b"aaaa")).await;
            h.send(now, fb, &check(b"bbbb")).await;
            let pb = recv(&b, &check(b"bbbb")).await;
            // Game packets cannot use a provisional connection before the Bedrock server accepts ICE.
            h.send(now, fa, b"premature game packet").await;
            h.send(now, fa, &check(b"aaaa")).await;
            recv(&a, &check(b"aaaa")).await;
            h.reply(now, &a, pa, fa, &success()).await;
            h.reply(now, &b, pb, fb, &success()).await;

            // Encrypted traffic uses the flow even after the pending join expires.
            h.send(now + 20_000, fa, &check(b"xxxx")).await;
            h.send(now + 20_000, fa, b"DTLS A").await;
            recv(&a, b"DTLS A").await;
            h.send(now + 20_000, fb, b"DTLS B").await;
            recv(&b, b"DTLS B").await;
            h.reply(now + 20_000, &b, pb, fb, b"reply B").await;

            // A fresh join on A's old endpoint selects a new Bedrock server port.
            h.join(fa, c.local_addr().unwrap(), "cccc").await;
            h.send(now_milli(), fa, &check(b"cccc")).await;
            let pc = recv(&c, &check(b"cccc")).await;
            h.reply(now_milli(), &c, pc, fa, &success()).await;
            h.send(now_milli(), fa, b"DTLS C").await;
            recv(&c, b"DTLS C").await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn tls_probe_gets_fallback_alert_and_unrewritable_answer_is_not_forwarded() {
        timeout(WAIT, async {
            let (mut client, tunnel) = tcp_pair().await;
            let (origin, _server) = tcp_pair().await;
            let task = tokio::spawn(async move { nethernet::proxy_http_with_timeout(tunnel, origin, context(flow(41000)), &NetherNetSessions::new(), &AgentStats::new()).await });
            client.write_all(&[0x16,3,1,0,0]).await.unwrap();
            let mut alert = [0;7];
            client.read_exact(&mut alert).await.unwrap();
            assert_eq!(alert, [0x15,3,1,0,2,2,0x28]);
            assert_eq!(task.await.unwrap().unwrap(), JoinOutcome::TlsRefused);

            let (mut client, tunnel) = tcp_pair().await;
            let (origin, mut server) = tcp_pair().await;
            let task = tokio::spawn(async move { nethernet::proxy_http_with_timeout(tunnel, origin, context(flow(41000)), &NetherNetSessions::new(), &AgentStats::new()).await });
            client.write_all(b"POST /v1/join/1 HTTP/1.1\r\nContent-Type: application/sdp\r\nContent-Length: 5\r\n\r\nv=0\r\n").await.unwrap();
            HttpMessage::read(&mut server,1024,8192).await.unwrap();
            server.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/sdp\r\nContent-Length: 9\r\n\r\n127.0.0.1").await.unwrap();
            let mut bytes = Vec::new();
            client.read_to_end(&mut bytes).await.unwrap();
            assert!(bytes.is_empty());
            assert!(task.await.unwrap().is_err());
        }).await.unwrap();
    }
}
