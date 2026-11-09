use std::convert::Infallible;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::client::conn::http1::SendRequest;
use hyper::header::{self, HeaderMap, HeaderName, HeaderValue};
use hyper::{Request, Response, StatusCode, Uri, Version};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use playit_api_client::api::ProxyProtocol;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::time::Instant;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::sync::CancellationToken;

use super::manager::{ACME_CHALLENGE_PREFIX, CertManager};
use crate::network::lan_address::LanAddress;
use crate::network::proxy_protocol::ProxyProtocolHeader;
use crate::stats::AgentStats;

const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Upstream connections one tunnel connection may hold open at once.
const MAX_UPSTREAM_CONNECTIONS: usize = 16;
/// Upstream connections kept open between requests on one tunnel connection.
const MAX_IDLE_UPSTREAM: usize = 8;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type ProxyBody = BoxBody<Bytes, BoxError>;

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
    /// Stops the connection and every task it spawned, including upgraded connections.
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, Copy)]
struct Timeouts {
    h2_keepalive_interval: Duration,
    h2_keepalive_timeout: Duration,
    /// Plain tunnel connections are dropped after 60-90s without data. A terminated
    /// connection is closed once no request, request body, response or upgraded data
    /// has moved for this long, and a request whose body or response stalls this long
    /// fails on its own. HTTP/2 keepalive pings don't count, so they can't keep a stalled
    /// connection open.
    idle: Duration,
    /// How long a request waits for a free upstream connection once the cap is reached.
    upstream_wait: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            h2_keepalive_interval: Duration::from_secs(30),
            h2_keepalive_timeout: Duration::from_secs(20),
            idle: Duration::from_secs(90),
            upstream_wait: Duration::from_secs(30),
        }
    }
}

/// Terminates TLS on a tunnel connection and forwards its HTTP requests upstream.
///
/// Returns only once every task the connection spawned has ended, so callers can count
/// the connection as active for as long as this runs.
pub async fn serve_https<S>(tunnel: S, acceptor: TlsAcceptor, ctx: ProxyContext)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    serve_https_with(tunnel, acceptor, ctx, Timeouts::default()).await
}

async fn serve_https_with<S>(
    tunnel: S,
    acceptor: TlsAcceptor,
    ctx: ProxyContext,
    timeouts: Timeouts,
) where
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

    // A child token, so closing an idle connection doesn't cancel the caller's token.
    let cancel = ctx.cancel.child_token();
    let (alive, mut all_tasks_ended) = mpsc::channel::<()>(1);
    let executor = TaskExecutor {
        cancel: cancel.clone(),
        alive,
    };
    let forwarder = Arc::new(Forwarder::new(ctx, "https", executor.clone(), timeouts));
    tokio::spawn(close_when_idle(
        forwarder.activity.clone(),
        timeouts.idle,
        cancel.clone(),
    ));
    let service = hyper::service::service_fn(move |req| forwarder.clone().forward(req));

    let mut builder = auto::Builder::new(executor);
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT);
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(timeouts.h2_keepalive_interval)
        .keep_alive_timeout(timeouts.h2_keepalive_timeout);
    let connection = builder.serve_connection_with_upgrades(TokioIo::new(tls), service);
    if let Some(Err(error)) = cancel.run_until_cancelled(connection).await {
        tracing::debug!(%error, "HTTPS connection ended with an error");
    }
    drop(builder);

    // Each spawned task holds a sender; recv returns None once all of them are gone.
    let _ = all_tasks_ended.recv().await;
    // Stops the idle watchdog.
    cancel.cancel();
}

/// Cancels the connection once nothing has moved through it for `idle`.
async fn close_when_idle(activity: Arc<Activity>, idle: Duration, cancel: CancellationToken) {
    tokio::select! {
        _ = until_idle(&activity, idle) => {
            tracing::debug!(?idle, "closing HTTPS connection with no activity");
            cancel.cancel();
        }
        _ = cancel.cancelled() => {}
    }
}

/// Returns once `activity` hasn't been touched for `idle`.
async fn until_idle(activity: &Activity, idle: Duration) {
    loop {
        let deadline = activity.last() + idle;
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep_until(deadline).await;
    }
}

/// Spawns the connection's background tasks (HTTP/2 streams, upstream connections and
/// upgraded relays) so they stop on cancel and can be waited for.
#[derive(Clone)]
struct TaskExecutor {
    cancel: CancellationToken,
    alive: mpsc::Sender<()>,
}

impl TaskExecutor {
    fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let cancel = self.cancel.clone();
        let alive = self.alive.clone();
        tokio::spawn(async move {
            let _alive = alive;
            cancel.run_until_cancelled(task).await;
        });
    }
}

impl<F> hyper::rt::Executor<F> for TaskExecutor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, task: F) {
        self.spawn(async move {
            task.await;
        });
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
    executor: TaskExecutor,
    timeouts: Timeouts,
    /// Upstream connections kept open between requests on this tunnel connection.
    pool: Mutex<Vec<SendRequest<ProxyBody>>>,
    /// One permit per open upstream connection, held by its connection task.
    connections: Arc<Semaphore>,
    /// Touched whenever a request, body data or upgraded data moves on this connection.
    activity: Arc<Activity>,
}

impl Forwarder {
    fn new(
        ctx: ProxyContext,
        scheme: &'static str,
        executor: TaskExecutor,
        timeouts: Timeouts,
    ) -> Self {
        Forwarder {
            ctx,
            scheme,
            executor,
            timeouts,
            pool: Mutex::new(Vec::new()),
            connections: Arc::new(Semaphore::new(MAX_UPSTREAM_CONNECTIONS)),
            activity: Arc::new(Activity::new()),
        }
    }

    async fn forward(
        self: Arc<Self>,
        req: Request<Incoming>,
    ) -> Result<Response<ProxyBody>, Infallible> {
        match self.forward_inner(req).await {
            Ok(response) => Ok(response),
            Err(error) => {
                tracing::warn!(
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
        self.activity.touch();
        let upgrade = upgrade_protocol(&req);
        let client_upgrade = upgrade.is_some().then(|| hyper::upgrade::on(&mut req));

        let (mut parts, body) = req.into_parts();
        let host = parts
            .headers
            .get(header::HOST)
            .cloned()
            .or_else(|| {
                let authority = parts.uri.authority()?;
                HeaderValue::from_str(authority.as_str()).ok()
            })
            .or_else(|| HeaderValue::from_str(&self.ctx.domain).ok());

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
        drop_length_if_chunked(&mut parts.headers);
        strip_hop_by_hop(&mut parts.headers);
        if let Some(protocol) = &upgrade {
            parts
                .headers
                .insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
            parts.headers.insert(header::UPGRADE, protocol.clone());
        }
        set_forwarding_headers(
            &mut parts.headers,
            host,
            self.scheme,
            self.ctx.peer_addr.ip(),
        );

        // Touched by this request's body and response, so a stall in either fails it.
        let request_activity = Arc::new(Activity::new());
        let body = self.idle_body(body, &request_activity);
        let idle = self.timeouts.idle;
        let mut response = tokio::select! {
            response = self.send(Request::from_parts(parts, body)) => response?,
            _ = until_idle(&request_activity, idle) => {
                return Err(format!("the local server did not respond for {idle:?}"));
            }
        };
        request_activity.touch();
        self.activity.touch();

        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            let Some(client_upgrade) = client_upgrade else {
                return Err("the local server switched protocols without being asked".into());
            };
            let upstream_upgrade = hyper::upgrade::on(&mut response);
            let activity = self.activity.clone();
            self.executor.spawn(async move {
                let (client, upstream) = match tokio::try_join!(client_upgrade, upstream_upgrade) {
                    Ok(pair) => pair,
                    Err(error) => {
                        tracing::debug!(%error, "protocol upgrade failed");
                        return;
                    }
                };
                relay(TokioIo::new(client), TokioIo::new(upstream), activity).await;
            });
            return Ok(response.map(|body| self.idle_body(body, &request_activity)));
        }

        drop_length_if_chunked(response.headers_mut());
        strip_hop_by_hop(response.headers_mut());
        Ok(response.map(|body| self.idle_body(body, &request_activity)))
    }

    fn idle_body(&self, body: Incoming, request_activity: &Arc<Activity>) -> ProxyBody {
        IdleBody {
            inner: body,
            request: request_activity.clone(),
            connection: self.activity.clone(),
            idle: self.timeouts.idle,
            timer: Box::pin(tokio::time::sleep(self.timeouts.idle)),
        }
        .boxed()
    }

    async fn send(&self, req: Request<ProxyBody>) -> Result<Response<Incoming>, String> {
        let (sender, reused) = self.upstream_sender(true).await?;
        self.send_on(sender, reused, req).await
    }

    async fn send_on(
        &self,
        mut sender: SendRequest<ProxyBody>,
        reused: bool,
        req: Request<ProxyBody>,
    ) -> Result<Response<Incoming>, String> {
        let mut error = match sender.try_send_request(req).await {
            Ok(response) => {
                self.return_to_pool(sender);
                return Ok(response);
            }
            Err(error) => error,
        };

        // A pooled connection the local server closed while idle fails before the
        // request is written, and hyper hands the request back. Only then is it safe to
        // send it again.
        let req = match error.take_message() {
            Some(req) if reused => req,
            _ => return Err(format!("request failed: {}", error.into_error())),
        };
        tracing::debug!(error = %error.into_error(), "pooled upstream connection was closed; retrying on a new one");
        let (mut sender, _) = self.upstream_sender(false).await?;
        let response = sender
            .send_request(req)
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        self.return_to_pool(sender);
        Ok(response)
    }

    fn return_to_pool(&self, sender: SendRequest<ProxyBody>) {
        if sender.is_closed() {
            return;
        }
        let mut pool = self.pool.lock().unwrap();
        if pool.len() < MAX_IDLE_UPSTREAM {
            pool.push(sender);
        }
    }

    /// Takes a pooled connection that can send a request now. Connections still
    /// streaming a response stay in the pool for later.
    fn take_ready_sender(&self) -> Option<SendRequest<ProxyBody>> {
        let mut pool = self.pool.lock().unwrap();
        pool.retain(|sender| !sender.is_closed());
        let index = pool.iter().position(|sender| sender.is_ready())?;
        Some(pool.swap_remove(index))
    }

    /// Returns a connection to the local server and whether it came from the pool.
    async fn upstream_sender(
        &self,
        allow_pooled: bool,
    ) -> Result<(SendRequest<ProxyBody>, bool), String> {
        let deadline = Instant::now() + self.timeouts.upstream_wait;
        loop {
            if allow_pooled && let Some(sender) = self.take_ready_sender() {
                return Ok((sender, true));
            }
            if let Ok(permit) = self.connections.clone().try_acquire_owned() {
                return Ok((self.connect(permit).await?, false));
            }

            // Every connection is in use. Wait for one to close, and look at the pool
            // again now and then in case one finishes its response.
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "all {MAX_UPSTREAM_CONNECTIONS} connections to the local server are busy"
                ));
            }
            let wait = (deadline - now).min(Duration::from_millis(50));
            if let Ok(Ok(permit)) =
                tokio::time::timeout(wait, self.connections.clone().acquire_owned()).await
            {
                return Ok((self.connect(permit).await?, false));
            }
        }
    }

    async fn connect(
        &self,
        permit: OwnedSemaphorePermit,
    ) -> Result<SendRequest<ProxyBody>, String> {
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
        self.executor.spawn(async move {
            let _permit = permit;
            if let Err(error) = connection.with_upgrades().await {
                tracing::debug!(%error, "upstream connection closed with an error");
            }
        });
        Ok(sender)
    }
}

/// playit is the first proxy in front of the local server, so forwarding headers sent
/// by the client are replaced rather than trusted or appended to.
fn set_forwarding_headers(
    headers: &mut HeaderMap,
    host: Option<HeaderValue>,
    scheme: &'static str,
    peer_ip: IpAddr,
) {
    headers.remove(header::FORWARDED);
    let peer =
        HeaderValue::from_str(&peer_ip.to_string()).expect("an IP address is a valid header value");
    headers.insert(HeaderName::from_static("x-forwarded-for"), peer.clone());
    headers.insert(HeaderName::from_static("x-real-ip"), peer);
    headers.insert(
        HeaderName::from_static("x-forwarded-proto"),
        HeaderValue::from_static(scheme),
    );
    match host {
        Some(host) => {
            headers.insert(header::HOST, host.clone());
            headers.insert(HeaderName::from_static("x-forwarded-host"), host);
        }
        None => {
            headers.remove(HeaderName::from_static("x-forwarded-host"));
        }
    }
}

/// hyper decodes a chunked body but keeps a Content-Length sent alongside it. Once
/// Transfer-Encoding is stripped as hop-by-hop, that stale length would misframe the
/// forwarded body.
fn drop_length_if_chunked(headers: &mut HeaderMap) {
    if headers.contains_key(header::TRANSFER_ENCODING) {
        headers.remove(header::CONTENT_LENGTH);
    }
}

/// Copies bytes between an upgraded client and upstream connection until either side
/// closes. Data moving touches the connection's activity; the connection's idle
/// watchdog ends the relay once nothing has moved for a while.
async fn relay<A, B>(client: A, upstream: B, activity: Arc<Activity>)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let mut client = ActivityIo {
        inner: client,
        activity: activity.clone(),
    };
    let mut upstream = ActivityIo {
        inner: upstream,
        activity,
    };
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
}

/// A request or response body that fails once no data has arrived for `idle` while it
/// is being read. Data touches both the request's and the connection's activity.
///
/// The timer only runs while the body is being read, so a peer that is slow to accept
/// data isn't blamed on this body; the connection's idle watchdog covers that case.
struct IdleBody {
    inner: Incoming,
    request: Arc<Activity>,
    connection: Arc<Activity>,
    idle: Duration,
    timer: Pin<Box<tokio::time::Sleep>>,
}

impl Body for IdleBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                this.request.touch();
                this.connection.touch();
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error.into()))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                let deadline = this.request.last() + this.idle;
                this.timer.as_mut().reset(deadline);
                if this.timer.as_mut().poll(cx).is_ready() {
                    let idle = this.idle;
                    return Poll::Ready(Some(Err(format!("no body data for {idle:?}").into())));
                }
                Poll::Pending
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// When data last moved through a connection.
struct Activity {
    start: Instant,
    elapsed_ms: AtomicU64,
}

impl Activity {
    fn new() -> Self {
        Activity {
            start: Instant::now(),
            elapsed_ms: AtomicU64::new(0),
        }
    }

    fn touch(&self) {
        let elapsed = self.start.elapsed().as_millis() as u64;
        self.elapsed_ms.store(elapsed, Ordering::Relaxed);
    }

    fn last(&self) -> Instant {
        self.start + Duration::from_millis(self.elapsed_ms.load(Ordering::Relaxed))
    }
}

struct ActivityIo<S> {
    inner: S,
    activity: Arc<Activity>,
}

impl<S: AsyncRead + Unpin> AsyncRead for ActivityIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.activity.touch();
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for ActivityIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.activity.touch();
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
