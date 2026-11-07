use super::*;
use crate::tls::manager::build_acceptor;
use crate::tls::store::{CertStore, StoredCert};
use http_body_util::Empty;
use std::sync::atomic::AtomicUsize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_rustls::client::TlsStream;

const PEER: &str = "203.0.113.7:51000";
const WAIT: Duration = Duration::from_secs(5);

fn self_signed(domain: &str) -> StoredCert {
    let cert = rcgen::generate_simple_self_signed(vec![domain.to_string()]).unwrap();
    StoredCert {
        chain_pem: cert.cert.pem(),
        key_pem: cert.signing_key.serialize_pem(),
    }
}

fn context(upstream: Upstream) -> ProxyContext {
    ProxyContext {
        domain: "site.example.com".into(),
        upstream,
        peer_addr: PEER.parse().unwrap(),
        proxy_protocol: None,
        stats: None,
        cancel: CancellationToken::new(),
    }
}

fn short_timeouts() -> Timeouts {
    Timeouts {
        h2_keepalive_interval: Duration::from_millis(100),
        h2_keepalive_timeout: Duration::from_millis(100),
        upgrade_idle: Duration::from_millis(200),
        upstream_wait: Duration::from_millis(200),
    }
}

/// Starts the proxy on one end of an in-memory tunnel and connects a TLS client to it.
async fn start_proxy(
    ctx: ProxyContext,
    timeouts: Timeouts,
    alpn: &[u8],
) -> (TlsStream<tokio::io::DuplexStream>, JoinHandle<()>) {
    let (client_side, tunnel_side) = tokio::io::duplex(64 * 1024);
    let (acceptor, _) = build_acceptor(&self_signed("site.example.com")).unwrap();
    let handle = tokio::spawn(serve_https_with(tunnel_side, acceptor, ctx, timeouts));

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
        .with_no_client_auth();
    config.alpn_protocols = vec![alpn.to_vec()];
    let tls = TlsConnector::from(Arc::new(config))
        .connect(
            ServerName::try_from("site.example.com").unwrap(),
            client_side,
        )
        .await
        .unwrap();
    (tls, handle)
}

/// An upstream that echoes the request line, forwarding headers and body in its body.
async fn echo_upstream(tls: bool) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = tls.then(|| build_acceptor(&self_signed("upstream.local")).unwrap().0);

    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(|req: Request<Incoming>| async move {
                    let h = |name: &str| {
                        let values: Vec<&str> = req
                            .headers()
                            .get_all(name)
                            .iter()
                            .map(|v| v.to_str().unwrap())
                            .collect();
                        if values.is_empty() {
                            "-".to_string()
                        } else {
                            values.join("|")
                        }
                    };
                    let head = format!(
                        "{} {} host={} xff={} xri={} xfp={} xfh={} fwd={} conn={} cookie={} cl={}",
                        req.method(),
                        req.uri(),
                        h("host"),
                        h("x-forwarded-for"),
                        h("x-real-ip"),
                        h("x-forwarded-proto"),
                        h("x-forwarded-host"),
                        h("forwarded"),
                        h("connection"),
                        h("cookie"),
                        h("content-length"),
                    );
                    let body = req.into_body().collect().await.unwrap().to_bytes();
                    let text = format!("{head} body={}", String::from_utf8_lossy(&body));
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(text))))
                });
                let builder = hyper::server::conn::http1::Builder::new();
                match acceptor {
                    Some(acceptor) => {
                        let tls = acceptor.accept(tcp).await.unwrap();
                        let _ = builder.serve_connection(TokioIo::new(tls), service).await;
                    }
                    None => {
                        let _ = builder.serve_connection(TokioIo::new(tcp), service).await;
                    }
                }
            });
        }
    });
    addr
}

async fn get_through_tls_tunnel(
    upstream: Upstream,
    requests: Vec<Request<Empty<Bytes>>>,
) -> Vec<(StatusCode, String)> {
    let (tls, _) = start_proxy(context(upstream), Timeouts::default(), b"http/1.1").await;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .unwrap();
    tokio::spawn(conn);

    let mut results = Vec::new();
    for req in requests {
        let response = sender.send_request(req).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        results.push((status, String::from_utf8(body.to_vec()).unwrap()));
    }
    results
}

fn get(path: &str) -> Request<Empty<Bytes>> {
    Request::get(path)
        .header(header::HOST, "site.example.com")
        .header(header::CONNECTION, "keep-alive")
        .body(Empty::new())
        .unwrap()
}

/// Reads from `stream` until the end of an HTTP head.
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> String {
    let mut buf = vec![0u8; 4096];
    let mut read = 0;
    while !buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf[read..]).await.unwrap();
        assert!(n > 0, "connection closed before the end of the head");
        read += n;
    }
    String::from_utf8_lossy(&buf[..read]).into_owned()
}

/// Reads until the peer closes the connection or it fails.
async fn read_until_closed<S: AsyncRead + Unpin>(stream: &mut S) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return out,
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
}

#[tokio::test]
async fn forwards_decrypted_requests_to_http_upstream() {
    let addr = echo_upstream(false).await;
    let results =
        get_through_tls_tunnel(Upstream::Http(addr), vec![get("/a?x=1"), get("/b")]).await;

    assert_eq!(results[0].0, StatusCode::OK);
    assert_eq!(
        results[0].1,
        "GET /a?x=1 host=site.example.com xff=203.0.113.7 xri=203.0.113.7 xfp=https xfh=site.example.com fwd=- conn=- cookie=- cl=- body="
    );
    assert!(results[1].1.starts_with("GET /b "));
}

#[tokio::test]
async fn forwards_decrypted_requests_to_https_upstream() {
    let addr = echo_upstream(true).await;
    let results = get_through_tls_tunnel(Upstream::Https(addr), vec![get("/secure")]).await;

    assert_eq!(results[0].0, StatusCode::OK);
    assert!(
        results[0]
            .1
            .starts_with("GET /secure host=site.example.com")
    );
}

#[tokio::test]
async fn client_forwarding_headers_are_replaced() {
    let addr = echo_upstream(false).await;
    let spoofed = Request::get("/")
        .header(header::HOST, "site.example.com")
        .header("x-forwarded-for", "10.0.0.1")
        .header("x-forwarded-for", "10.0.0.2")
        .header("x-real-ip", "10.0.0.3")
        .header("x-forwarded-proto", "http")
        .header("x-forwarded-host", "evil.example.com")
        .header(header::FORWARDED, "for=10.0.0.4;proto=http")
        .body(Empty::new())
        .unwrap();
    let results = get_through_tls_tunnel(Upstream::Http(addr), vec![spoofed]).await;

    assert_eq!(
        results[0].1,
        "GET / host=site.example.com xff=203.0.113.7 xri=203.0.113.7 xfp=https xfh=site.example.com fwd=- conn=- cookie=- cl=- body="
    );
}

#[tokio::test]
async fn chunked_request_with_content_length_is_forwarded_intact() {
    let addr = echo_upstream(false).await;
    let (mut tls, _) = start_proxy(
        context(Upstream::Http(addr)),
        Timeouts::default(),
        b"http/1.1",
    )
    .await;

    tls.write_all(
        b"POST /upload HTTP/1.1\r\nHost: site.example.com\r\nContent-Length: 3\r\n\
          Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
          5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
    )
    .await
    .unwrap();
    let response = String::from_utf8(read_until_closed(&mut tls).await).unwrap();

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.ends_with("POST /upload host=site.example.com xff=203.0.113.7 xri=203.0.113.7 xfp=https xfh=site.example.com fwd=- conn=- cookie=- cl=- body=hello world"),
        "{response}"
    );
}

#[tokio::test]
async fn unreachable_upstream_returns_bad_gateway() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let results = get_through_tls_tunnel(Upstream::Http(addr), vec![get("/")]).await;
    assert_eq!(results[0].0, StatusCode::BAD_GATEWAY);
}

/// An upstream that accepts one WebSocket upgrade, echoes raw bytes, and reports when
/// the proxy closes the connection.
async fn websocket_upstream() -> (SocketAddr, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let request = read_head(&mut tcp).await.to_ascii_lowercase();
        assert!(request.contains("upgrade: websocket"));
        assert!(request.contains("connection: upgrade"));
        tcp.write_all(
            b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: websocket\r\n\r\n",
        )
        .await
        .unwrap();
        let mut buf = [0u8; 1024];
        loop {
            match tcp.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => tcp.write_all(&buf[..n]).await.unwrap(),
            }
        }
        let _ = closed_tx.send(());
    });
    (addr, closed_rx)
}

/// Upgrades a proxied connection to a WebSocket and checks that bytes are relayed.
async fn open_websocket(
    ctx: ProxyContext,
    timeouts: Timeouts,
) -> (TlsStream<tokio::io::DuplexStream>, JoinHandle<()>) {
    let (mut tls, handle) = start_proxy(ctx, timeouts, b"http/1.1").await;
    tls.write_all(
        b"GET /ws HTTP/1.1\r\nHost: site.example.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
    )
    .await
    .unwrap();
    assert!(read_head(&mut tls).await.starts_with("HTTP/1.1 101"));

    tls.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 16];
    let n = tls.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"ping");
    (tls, handle)
}

#[tokio::test]
async fn websocket_upgrade_is_relayed() {
    let (addr, _) = websocket_upstream().await;
    open_websocket(context(Upstream::Http(addr)), Timeouts::default()).await;
}

#[tokio::test]
async fn cancelling_the_session_closes_upgraded_connections() {
    let (addr, upstream_closed) = websocket_upstream().await;
    let ctx = context(Upstream::Http(addr));
    let cancel = ctx.cancel.clone();
    let (mut tls, handle) = open_websocket(ctx, Timeouts::default()).await;

    // The relay keeps serve_https running, which keeps the connection counted.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!handle.is_finished());

    cancel.cancel();
    tokio::time::timeout(WAIT, handle).await.unwrap().unwrap();
    tokio::time::timeout(WAIT, upstream_closed)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(WAIT, read_until_closed(&mut tls))
        .await
        .unwrap();
}

#[tokio::test]
async fn idle_upgraded_connections_are_closed() {
    let (addr, upstream_closed) = websocket_upstream().await;
    let (mut tls, handle) = open_websocket(context(Upstream::Http(addr)), short_timeouts()).await;

    tokio::time::timeout(WAIT, handle).await.unwrap().unwrap();
    tokio::time::timeout(WAIT, upstream_closed)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(WAIT, read_until_closed(&mut tls))
        .await
        .unwrap();
}

#[tokio::test]
async fn unresponsive_http2_clients_are_dropped() {
    let addr = echo_upstream(false).await;
    let (mut tls, handle) =
        start_proxy(context(Upstream::Http(addr)), short_timeouts(), b"h2").await;

    // The connection preface and an empty SETTINGS frame, then never read the server's
    // keepalive pings.
    tls.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00")
        .await
        .unwrap();

    tokio::time::timeout(WAIT, handle).await.unwrap().unwrap();
}

#[test]
fn plain_http_answers_challenges_and_redirects() {
    let certs = CertManager::new(CertStore::new(std::env::temp_dir().join("unused")));

    let redirect = plain_http_response(
        &certs,
        "site.example.com",
        &"/page?q=1".parse::<Uri>().unwrap(),
    );
    assert_eq!(redirect.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        redirect.headers()[header::LOCATION],
        "https://site.example.com/page?q=1"
    );

    let unknown = plain_http_response(
        &certs,
        "site.example.com",
        &"/.well-known/acme-challenge/nope".parse::<Uri>().unwrap(),
    );
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

async fn http2_client(ctx: ProxyContext) -> hyper::client::conn::http2::SendRequest<Empty<Bytes>> {
    let (tls, _) = start_proxy(ctx, Timeouts::default(), b"h2").await;
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (sender, conn) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        TokioIo::new(tls),
    )
    .await
    .unwrap();
    tokio::spawn(conn);
    sender
}

#[tokio::test]
async fn http2_clients_are_forwarded_as_http1() {
    let addr = echo_upstream(false).await;
    let mut sender = http2_client(context(Upstream::Http(addr))).await;

    let req = Request::get("https://site.example.com/h2?q=1")
        .header(header::COOKIE, "a=1")
        .header(header::COOKIE, "b=2")
        .header("x-forwarded-for", "10.0.0.1")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let response = sender.send_request(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        "GET /h2?q=1 host=site.example.com xff=203.0.113.7 xri=203.0.113.7 xfp=https xfh=site.example.com fwd=- conn=- cookie=a=1; b=2 cl=- body="
    );
}

/// Captures what the proxy sends to an upstream before the end of the request head.
async fn capture_upstream_bytes(protocol: ProxyProtocol) -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (captured_tx, captured_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let mut read = 0;
        while !buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
            read += tcp.read(&mut buf[read..]).await.unwrap();
        }
        tcp.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
        let _ = captured_tx.send(buf[..read].to_vec());
        let _ = read_until_closed(&mut tcp).await;
    });

    let mut ctx = context(Upstream::Http(addr));
    ctx.proxy_protocol = Some((
        protocol,
        ProxyProtocolHeader::AfInet {
            client_ip: "203.0.113.7".parse().unwrap(),
            proxy_ip: "198.51.100.1".parse().unwrap(),
            client_port: 51000,
            proxy_port: 443,
        },
    ));
    let (tls, _) = start_proxy(ctx, Timeouts::default(), b"http/1.1").await;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .unwrap();
    tokio::spawn(conn);
    let response = sender.send_request(get("/")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    tokio::time::timeout(WAIT, captured_rx)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn proxy_protocol_v1_header_precedes_the_request() {
    let bytes = capture_upstream_bytes(ProxyProtocol::ProxyProtocolV1).await;
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        text.starts_with("PROXY TCP4 203.0.113.7 198.51.100.1 51000 443\r\nGET / HTTP/1.1\r\n"),
        "{text}"
    );
}

#[tokio::test]
async fn proxy_protocol_v2_header_precedes_the_request() {
    let bytes = capture_upstream_bytes(ProxyProtocol::ProxyProtocolV2).await;
    assert_eq!(&bytes[..12], b"\r\n\r\n\0\r\nQUIT\n");
    // Version/command, family, length, then two IPv4 addresses and two ports.
    assert_eq!(bytes[13], 0x11);
    assert_eq!(&bytes[16..20], &[203, 0, 113, 7]);
    assert_eq!(&bytes[20..24], &[198, 51, 100, 1]);
    assert_eq!(&bytes[24..26], &51000u16.to_be_bytes());
    assert_eq!(&bytes[26..28], &443u16.to_be_bytes());
    assert!(bytes[28..].starts_with(b"GET / HTTP/1.1\r\n"));
}

fn test_forwarder(upstream: Upstream, timeouts: Timeouts) -> Forwarder {
    let (alive, _) = mpsc::channel(1);
    let executor = TaskExecutor {
        cancel: CancellationToken::new(),
        alive,
    };
    Forwarder::new(context(upstream), "https", executor, timeouts)
}

fn proxy_request(path: &str) -> Request<ProxyBody> {
    Request::get(path)
        .header(header::HOST, "site.example.com")
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap()
}

/// A sender whose connection is already gone, like a pooled connection the local
/// server closed while it sat idle.
async fn closed_sender() -> SendRequest<ProxyBody> {
    let (io, _peer) = tokio::io::duplex(1024);
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .unwrap();
    drop(connection);
    sender
}

async fn live_sender(addr: SocketAddr) -> SendRequest<ProxyBody> {
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
        .await
        .unwrap();
    tokio::spawn(connection);
    sender.ready().await.unwrap();
    sender
}

#[tokio::test]
async fn closed_pooled_connection_is_retried_on_a_new_one() {
    let addr = echo_upstream(false).await;
    let forwarder = test_forwarder(Upstream::Http(addr), Timeouts::default());

    let response = forwarder
        .send_on(closed_sender().await, true, proxy_request("/again"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(body.starts_with(b"GET /again "));
}

#[tokio::test]
async fn failed_fresh_connection_is_not_retried() {
    let addr = echo_upstream(false).await;
    let forwarder = test_forwarder(Upstream::Http(addr), Timeouts::default());

    let result = forwarder
        .send_on(closed_sender().await, false, proxy_request("/"))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn busy_pooled_connection_does_not_hide_ready_ones() {
    // An upstream that accepts connections and never answers.
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_addr = silent.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            held.push(silent.accept().await.unwrap().0);
        }
    });
    let mut busy = live_sender(silent_addr).await;
    let pending = busy.send_request(proxy_request("/slow"));
    tokio::spawn(pending);
    assert!(!busy.is_ready());

    let addr = echo_upstream(false).await;
    let forwarder = test_forwarder(Upstream::Http(addr), Timeouts::default());
    let ready = live_sender(addr).await;
    forwarder.pool.lock().unwrap().extend([ready, busy]);

    let taken = forwarder.take_ready_sender().unwrap();
    assert!(taken.is_ready());
    assert_eq!(forwarder.pool.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn idle_pool_is_capped() {
    let addr = echo_upstream(false).await;
    let forwarder = test_forwarder(Upstream::Http(addr), Timeouts::default());
    for _ in 0..MAX_IDLE_UPSTREAM + 3 {
        forwarder.return_to_pool(live_sender(addr).await);
    }
    assert_eq!(forwarder.pool.lock().unwrap().len(), MAX_IDLE_UPSTREAM);
}

#[tokio::test]
async fn waits_for_a_free_upstream_connection() {
    let addr = echo_upstream(false).await;
    let forwarder = test_forwarder(Upstream::Http(addr), short_timeouts());
    let held = forwarder
        .connections
        .clone()
        .acquire_many_owned(MAX_UPSTREAM_CONNECTIONS as u32)
        .await
        .unwrap();

    let error = forwarder.upstream_sender(true).await.unwrap_err();
    assert!(error.contains("busy"), "{error}");

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(held);
    });
    let (_, reused) = forwarder.upstream_sender(true).await.unwrap();
    assert!(!reused);
}

#[tokio::test]
async fn concurrent_upstream_connections_are_capped() {
    // An upstream that answers slowly and records how many connections it has open.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let open = Arc::new(AtomicUsize::new(0));
    let most_open = Arc::new(AtomicUsize::new(0));
    {
        let open = open.clone();
        let most_open = most_open.clone();
        tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let now_open = open.fetch_add(1, Ordering::SeqCst) + 1;
                most_open.fetch_max(now_open, Ordering::SeqCst);
                let open = open.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(|_req: Request<Incoming>| async {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                    open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
    }

    let sender = http2_client(context(Upstream::Http(addr))).await;
    let requests = (0..MAX_UPSTREAM_CONNECTIONS + 8).map(|i| {
        let mut sender = sender.clone();
        async move {
            let req = Request::get(format!("https://site.example.com/{i}"))
                .body(Empty::<Bytes>::new())
                .unwrap();
            let response = sender.send_request(req).await.unwrap();
            let status = response.status();
            response.into_body().collect().await.unwrap();
            status
        }
    });
    let statuses = futures_util::future::join_all(requests).await;

    assert!(
        statuses.iter().all(|s| *s == StatusCode::OK),
        "{statuses:?}"
    );
    assert!(most_open.load(Ordering::SeqCst) <= MAX_UPSTREAM_CONNECTIONS);
}
