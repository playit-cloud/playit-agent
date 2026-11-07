use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper::header::{self, HeaderMap, HeaderName, HeaderValue};
use hyper::{Request, Response, StatusCode, Uri, Version};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use playit_api_client::api::ProxyProtocol;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use super::manager::{ACME_CHALLENGE_PREFIX, CertManager};
use crate::network::lan_address::LanAddress;
use crate::network::proxy_protocol::ProxyProtocolHeader;
use crate::stats::AgentStats;

const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

type ProxyBody = BoxBody<Bytes, hyper::Error>;

/// The local server that decrypted HTTPS requests are forwarded to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upstream {
    Http(SocketAddr),
    Https(SocketAddr),
}

/// Everything needed to forward one tunnel connection's requests.
pub struct ProxyContext {
    /// The tunnel's domain, used for SNI to an HTTPS upstream and for redirects.
    pub domain: String,
    pub upstream: Upstream,
    pub peer_addr: SocketAddr,
    pub proxy_protocol: Option<(ProxyProtocol, ProxyProtocolHeader)>,
    pub stats: Option<AgentStats>,
}

/// Terminates TLS on a tunnel connection and forwards its HTTP requests upstream.
pub async fn serve_https<S>(tunnel: S, acceptor: TlsAcceptor, ctx: ProxyContext)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let tunnel = CountingIo::new(tunnel, ctx.stats.clone());
    let tls = match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(tunnel)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(error)) => {
            tracing::debug!(%error, peer = %ctx.peer_addr, "TLS handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!(peer = %ctx.peer_addr, "TLS handshake timed out");
            return;
        }
    };

    let forwarder = Arc::new(Forwarder {
        ctx,
        scheme: "https",
        idle: Mutex::new(Vec::new()),
    });
    let service = hyper::service::service_fn(move |req| forwarder.clone().forward(req));

    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT);
    if let Err(error) = builder
        .serve_connection_with_upgrades(TokioIo::new(tls), service)
        .await
    {
        tracing::debug!(%error, "HTTPS connection ended with an error");
    }
}

/// Serves the tunnel's plain HTTP port: answers Let's Encrypt challenges and redirects
/// everything else to HTTPS.
pub async fn serve_http_redirect<S>(
    tunnel: S,
    certs: CertManager,
    domain: String,
    stats: Option<AgentStats>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let tunnel = CountingIo::new(tunnel, stats);
    let service = hyper::service::service_fn(move |req: Request<Incoming>| {
        let response = plain_http_response(&certs, &domain, req.uri());
        async move { Ok::<_, Infallible>(response) }
    });

    if let Err(error) = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT)
        .serve_connection(TokioIo::new(tunnel), service)
        .await
    {
        tracing::debug!(%error, "HTTP connection ended with an error");
    }
}

fn plain_http_response(certs: &CertManager, domain: &str, uri: &Uri) -> Response<Full<Bytes>> {
    if let Some(token) = uri.path().strip_prefix(ACME_CHALLENGE_PREFIX) {
        return match certs.challenge_response(token) {
            Some(key_auth) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Full::new(Bytes::from(key_auth)))
                .unwrap(),
            None => text_response(StatusCode::NOT_FOUND, "Unknown challenge"),
        };
    }

    // The tunnel's own domain is used rather than the Host header so the redirect can't
    // be pointed elsewhere.
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    Response::builder()
        .status(StatusCode::PERMANENT_REDIRECT)
        .header(header::LOCATION, format!("https://{domain}{path}"))
        .body(Full::new(Bytes::new()))
        .unwrap()
}

fn text_response(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(message.to_string())))
        .unwrap()
}

struct Forwarder {
    ctx: ProxyContext,
    scheme: &'static str,
    /// Upstream connections kept open between requests on this tunnel connection.
    idle: Mutex<Vec<SendRequest<Incoming>>>,
}

impl Forwarder {
    async fn forward(
        self: Arc<Self>,
        req: Request<Incoming>,
    ) -> Result<Response<ProxyBody>, Infallible> {
        match self.forward_inner(req).await {
            Ok(response) => Ok(response),
            Err(error) => {
                tracing::error!(
                    %error,
                    upstream = ?self.ctx.upstream,
                    domain = %self.ctx.domain,
                    "failed to forward HTTPS request to the local server; check that it is running on the configured port"
                );
                Ok(text_response(
                    StatusCode::BAD_GATEWAY,
                    "playit could not reach the local web server",
                )
                .map(|body| body.map_err(|never| match never {}).boxed()))
            }
        }
    }

    async fn forward_inner(
        &self,
        mut req: Request<Incoming>,
    ) -> Result<Response<ProxyBody>, String> {
        let upgrade = upgrade_protocol(&req);
        let client_upgrade = upgrade.is_some().then(|| hyper::upgrade::on(&mut req));

        let (mut parts, body) = req.into_parts();
        let host = parts
            .headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(str::to_string)
            .or_else(|| parts.uri.authority().map(|a| a.to_string()))
            .unwrap_or_else(|| self.ctx.domain.clone());

        parts.uri = parts
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
            .parse()
            .map_err(|e| format!("invalid request path: {e}"))?;
        if parts.version == Version::HTTP_2 {
            join_cookies(&mut parts.headers);
        }
        parts.version = Version::HTTP_11;
        strip_hop_by_hop(&mut parts.headers);
        let headers = &mut parts.headers;
        if let Some(protocol) = &upgrade {
            headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
            headers.insert(header::UPGRADE, protocol.clone());
        }
        if let Ok(value) = HeaderValue::from_str(&host) {
            headers.insert(header::HOST, value.clone());
            headers.insert(HeaderName::from_static("x-forwarded-host"), value);
        }
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static(self.scheme),
        );
        let peer_ip = self.ctx.peer_addr.ip().to_string();
        let forwarded_for = match headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            Some(existing) => format!("{existing}, {peer_ip}"),
            None => peer_ip,
        };
        if let Ok(value) = HeaderValue::from_str(&forwarded_for) {
            headers.insert(HeaderName::from_static("x-forwarded-for"), value);
        }

        let mut sender = self.upstream_sender().await?;
        let mut response = sender
            .send_request(Request::from_parts(parts, body))
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        if !sender.is_closed() {
            self.idle.lock().unwrap().push(sender);
        }

        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            let Some(client_upgrade) = client_upgrade else {
                return Err("the local server switched protocols without being asked".into());
            };
            let upstream_upgrade = hyper::upgrade::on(&mut response);
            tokio::spawn(async move {
                let (client, upstream) = match tokio::try_join!(client_upgrade, upstream_upgrade) {
                    Ok(pair) => pair,
                    Err(error) => {
                        tracing::debug!(%error, "protocol upgrade failed");
                        return;
                    }
                };
                let mut client = TokioIo::new(client);
                let mut upstream = TokioIo::new(upstream);
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
            return Ok(response.map(|body| body.boxed()));
        }

        strip_hop_by_hop(response.headers_mut());
        Ok(response.map(|body| body.boxed()))
    }

    async fn upstream_sender(&self) -> Result<SendRequest<Incoming>, String> {
        {
            let mut idle = self.idle.lock().unwrap();
            while let Some(sender) = idle.pop() {
                if sender.is_ready() {
                    return Ok(sender);
                }
                if !sender.is_closed() {
                    // Still busy with a streaming response; keep it for later.
                    idle.insert(0, sender);
                    break;
                }
            }
        }

        let (addr, tls) = match self.ctx.upstream {
            Upstream::Http(addr) => (addr, false),
            Upstream::Https(addr) => (addr, true),
        };

        let mut tcp = tokio::time::timeout(
            UPSTREAM_CONNECT_TIMEOUT,
            LanAddress::tcp_socket(true, self.ctx.peer_addr, addr),
        )
        .await
        .map_err(|_| format!("timed out connecting to {addr}"))?
        .map_err(|e| format!("could not connect to {addr}: {e}"))?;
        let _ = tcp.set_nodelay(true);

        if let Some((version, header)) = &self.ctx.proxy_protocol {
            let result = match version {
                ProxyProtocol::ProxyProtocolV1 => header.write_v1_tcp(&mut tcp).await,
                ProxyProtocol::ProxyProtocolV2 => header.write_v2_tcp(&mut tcp).await,
            };
            result.map_err(|e| format!("could not write the proxy protocol header: {e}"))?;
        }

        let io: Box<dyn Io> = if tls {
            let server_name = ServerName::try_from(self.ctx.domain.clone())
                .map_err(|e| format!("invalid server name: {e}"))?;
            let stream = tokio::time::timeout(
                TLS_HANDSHAKE_TIMEOUT,
                upstream_tls_connector().connect(server_name, tcp),
            )
            .await
            .map_err(|_| format!("TLS handshake with {addr} timed out"))?
            .map_err(|e| format!("TLS handshake with {addr} failed: {e}"))?;
            Box::new(stream)
        } else {
            Box::new(tcp)
        };

        let (sender, connection) = hyper::client::conn::http1::Builder::new()
            .handshake(TokioIo::new(io))
            .await
            .map_err(|e| format!("HTTP handshake with {addr} failed: {e}"))?;
        tokio::spawn(async move {
            if let Err(error) = connection.with_upgrades().await {
                tracing::debug!(%error, "upstream connection closed with an error");
            }
        });
        Ok(sender)
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// The protocol a client asked to upgrade to, for HTTP/1.1 upgrade requests such as WebSockets.
fn upgrade_protocol<B>(req: &Request<B>) -> Option<HeaderValue> {
    if req.version() != Version::HTTP_11 {
        return None;
    }
    let asks_upgrade = req
        .headers()
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
    if !asks_upgrade {
        return None;
    }
    req.headers().get(header::UPGRADE).cloned()
}

/// HTTP/2 may send each cookie as its own header, but HTTP/1.1 servers expect one
/// `Cookie` header (RFC 9113 section 8.2.3).
fn join_cookies(headers: &mut HeaderMap) {
    let cookies: Vec<&str> = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    if cookies.len() < 2 {
        return;
    }
    if let Ok(joined) = HeaderValue::from_str(&cookies.join("; ")) {
        headers.insert(header::COOKIE, joined);
    }
}

/// Removes headers that describe a single hop, including any the Connection header names.
fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|token| HeaderName::from_bytes(token.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in [
        header::CONNECTION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
        header::PROXY_AUTHORIZATION,
        HeaderName::from_static("keep-alive"),
        HeaderName::from_static("proxy-connection"),
    ] {
        headers.remove(name);
    }
}

/// Local HTTPS servers almost always use self-signed certificates, so the upstream
/// certificate isn't verified. The connection never leaves this machine or its LAN.
fn upstream_tls_connector() -> TlsConnector {
    static CONNECTOR: std::sync::OnceLock<TlsConnector> = std::sync::OnceLock::new();
    CONNECTOR
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let mut config = ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .expect("ring supports the default TLS versions")
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
                .with_no_client_auth();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            TlsConnector::from(Arc::new(config))
        })
        .clone()
}

#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Counts the bytes crossing the tunnel side of a connection in the agent's stats.
struct CountingIo<S> {
    inner: S,
    stats: Option<AgentStats>,
}

impl<S> CountingIo<S> {
    fn new(inner: S, stats: Option<AgentStats>) -> Self {
        CountingIo { inner, stats }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for CountingIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let (Poll::Ready(Ok(())), Some(stats)) = (&result, &self.stats) {
            stats.add_bytes_in((buf.filled().len() - before) as u64);
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CountingIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let (Poll::Ready(Ok(n)), Some(stats)) = (&result, &self.stats) {
            stats.add_bytes_out(*n as u64);
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests;
