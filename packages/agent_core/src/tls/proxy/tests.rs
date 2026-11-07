use super::*;
use crate::tls::manager::build_acceptor;
use crate::tls::store::{CertStore, StoredCert};
use http_body_util::Empty;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn self_signed(domain: &str) -> StoredCert {
    let cert = rcgen::generate_simple_self_signed(vec![domain.to_string()]).unwrap();
    StoredCert {
        chain_pem: cert.cert.pem(),
        key_pem: cert.signing_key.serialize_pem(),
    }
}

/// An upstream that echoes the request line and forwarding headers in its body.
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
                        req.headers()
                            .get(name)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("-")
                            .to_string()
                    };
                    let body = format!(
                        "{} {} host={} xff={} xfp={} xfh={} conn={} cookie={}",
                        req.method(),
                        req.uri(),
                        h("host"),
                        h("x-forwarded-for"),
                        h("x-forwarded-proto"),
                        h("x-forwarded-host"),
                        h("connection"),
                        h("cookie"),
                    );
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
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

async fn get_through_tls_tunnel(upstream: Upstream, paths: &[&str]) -> Vec<(StatusCode, String)> {
    let (client_side, tunnel_side) = tokio::io::duplex(64 * 1024);
    let (acceptor, _) = build_acceptor(&self_signed("site.example.com")).unwrap();
    tokio::spawn(serve_https(
        tunnel_side,
        acceptor,
        ProxyContext {
            domain: "site.example.com".into(),
            upstream,
            peer_addr: "203.0.113.7:51000".parse().unwrap(),
            proxy_protocol: None,
            stats: None,
        },
    ));

    let tls = upstream_tls_connector()
        .connect(
            ServerName::try_from("site.example.com").unwrap(),
            client_side,
        )
        .await
        .unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .unwrap();
    tokio::spawn(conn);

    let mut results = Vec::new();
    for path in paths {
        let req = Request::get(*path)
            .header(header::HOST, "site.example.com")
            .header("x-forwarded-for", "10.0.0.1")
            .header(header::CONNECTION, "keep-alive")
            .body(Empty::<Bytes>::new())
            .unwrap();
        let response = sender.send_request(req).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        results.push((status, String::from_utf8(body.to_vec()).unwrap()));
    }
    results
}

#[tokio::test]
async fn forwards_decrypted_requests_to_http_upstream() {
    let addr = echo_upstream(false).await;
    let results = get_through_tls_tunnel(Upstream::Http(addr), &["/a?x=1", "/b"]).await;

    assert_eq!(results[0].0, StatusCode::OK);
    assert_eq!(
        results[0].1,
        "GET /a?x=1 host=site.example.com xff=10.0.0.1, 203.0.113.7 xfp=https xfh=site.example.com conn=- cookie=-"
    );
    assert!(results[1].1.starts_with("GET /b "));
}

#[tokio::test]
async fn forwards_decrypted_requests_to_https_upstream() {
    let addr = echo_upstream(true).await;
    let results = get_through_tls_tunnel(Upstream::Https(addr), &["/secure"]).await;

    assert_eq!(results[0].0, StatusCode::OK);
    assert!(
        results[0]
            .1
            .starts_with("GET /secure host=site.example.com")
    );
}

#[tokio::test]
async fn unreachable_upstream_returns_bad_gateway() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let results = get_through_tls_tunnel(Upstream::Http(addr), &["/"]).await;
    assert_eq!(results[0].0, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn websocket_upgrade_is_relayed() {
    // An upstream that accepts the upgrade and then echoes raw bytes.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let mut read = 0;
        while !buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
            read += tcp.read(&mut buf[read..]).await.unwrap();
        }
        let request = String::from_utf8_lossy(&buf[..read]).to_ascii_lowercase();
        assert!(request.contains("upgrade: websocket"));
        assert!(request.contains("connection: upgrade"));
        tcp.write_all(
            b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: websocket\r\n\r\n",
        )
        .await
        .unwrap();
        let n = tcp.read(&mut buf).await.unwrap();
        tcp.write_all(&buf[..n]).await.unwrap();
    });

    let (client_side, tunnel_side) = tokio::io::duplex(64 * 1024);
    let (acceptor, _) = build_acceptor(&self_signed("site.example.com")).unwrap();
    tokio::spawn(serve_https(
        tunnel_side,
        acceptor,
        ProxyContext {
            domain: "site.example.com".into(),
            upstream: Upstream::Http(addr),
            peer_addr: "203.0.113.7:51000".parse().unwrap(),
            proxy_protocol: None,
            stats: None,
        },
    ));
    let mut tls = upstream_tls_connector()
        .connect(
            ServerName::try_from("site.example.com").unwrap(),
            client_side,
        )
        .await
        .unwrap();

    tls.write_all(
        b"GET /ws HTTP/1.1\r\nHost: site.example.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
    )
    .await
    .unwrap();
    let mut buf = vec![0u8; 4096];
    let mut read = 0;
    while !buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
        read += tls.read(&mut buf[read..]).await.unwrap();
    }
    assert!(buf.starts_with(b"HTTP/1.1 101"));

    tls.write_all(b"ping").await.unwrap();
    let n = tls.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"ping");
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

#[tokio::test]
async fn http2_clients_are_forwarded_as_http1() {
    let addr = echo_upstream(false).await;
    let (client_side, tunnel_side) = tokio::io::duplex(64 * 1024);
    let (acceptor, _) = build_acceptor(&self_signed("site.example.com")).unwrap();
    tokio::spawn(serve_https(
        tunnel_side,
        acceptor,
        ProxyContext {
            domain: "site.example.com".into(),
            upstream: Upstream::Http(addr),
            peer_addr: "203.0.113.7:51000".parse().unwrap(),
            proxy_protocol: None,
            stats: None,
        },
    ));

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let tls = TlsConnector::from(Arc::new(config))
        .connect(
            ServerName::try_from("site.example.com").unwrap(),
            client_side,
        )
        .await
        .unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));

    let (mut sender, conn) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .unwrap();
    tokio::spawn(conn);

    let req = Request::get("https://site.example.com/h2?q=1")
        .header(header::COOKIE, "a=1")
        .header(header::COOKIE, "b=2")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let response = sender.send_request(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        "GET /h2?q=1 host=site.example.com xff=203.0.113.7 xfp=https xfh=site.example.com conn=- cookie=a=1; b=2"
    );
}
