//! NetherNet signaling and UDP routing over an ordinary TCP+UDP tunnel.
//! The agent sanitizes SDP and keeps each session's Bedrock server port locally.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::stats::AgentStats;
use crate::utils::now_milli;

pub mod http_message;
pub mod sdp_answer;
pub mod sessions;
pub mod stun;
pub(crate) mod tcp;
pub mod udp_destination;
pub mod udp_state;

use self::http_message::{HttpError, HttpMessage};
use self::sdp_answer::{SdpAnswer, SdpAnswerError};
use self::sessions::NetherNetSessions;
use self::udp_destination::get_bedrock_udp_addr;

pub struct JoinContext {
    pub tunnel_id: u64,
    /// The public address the client connected to: what the answer must name.
    pub connect_addr: SocketAddr,
    pub peer_addr: SocketAddr,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum JoinOutcome {
    Status,
    Answer,
    TlsRefused,
}

#[derive(Debug)]
pub enum JoinError {
    /// The client's request could not be read, or was not NetherNet signaling.
    Request(HttpError),
    /// The Bedrock server could not be reached, or its reply could not be read.
    Origin(HttpError),
    /// The Bedrock server replied, but not with a 200 of the content type this request expects.
    UnexpectedReply,
    Answer(SdpAnswerError),
    TooManyJoins,
    WriteTunnel(std::io::Error),
    Timeout,
}

pub struct Limits;

impl Limits {
    /// The client sends five headers; the server sends three.
    pub const HEAD: usize = 1024;
    /// Offer with its signed identity token, captured at 2.7 KB; answer at 2.1 KB.
    pub const BODY: usize = 8 * 1024;
    /// One request and one reply, seconds apart at most.
    pub const EXCHANGE: Duration = Duration::from_secs(10);
}

/// Refuses the initial HTTPS probe so the Bedrock client retries with HTTP signaling.
struct HttpsRejection;

impl HttpsRejection {
    fn looks_like_https(data: &[u8]) -> bool {
        const TLS_HANDSHAKE_RECORD: u8 = 0x16;

        data.first() == Some(&TLS_HANDSHAKE_RECORD)
    }

    async fn write_rejection(tunn: &mut TcpStream, stats: &AgentStats) -> std::io::Result<()> {
        // TLS alert record, version 3.1, two-byte payload: fatal handshake_failure.
        const HANDSHAKE_FAILURE_ALERT: [u8; 7] = [0x15, 0x03, 0x01, 0, 2, 2, 0x28];

        tunn.write_all(&HANDSHAKE_FAILURE_ALERT).await?;
        stats.add_bytes_out(HANDSHAKE_FAILURE_ALERT.len() as u64);
        tunn.shutdown().await
    }
}

/// Proxies one join exchange between the claimed tunnel connection and the
/// Bedrock server's HTTP port, then closes both.
pub async fn proxy_http_with_timeout(
    tunn: TcpStream,
    origin: TcpStream,
    context: JoinContext,
    sessions: &NetherNetSessions,
    stats: &AgentStats,
) -> Result<JoinOutcome, JoinError> {
    match tokio::time::timeout(
        Limits::EXCHANGE,
        proxy_http(tunn, origin, context, sessions, stats),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(JoinError::Timeout),
    }
}

#[derive(Debug, PartialEq)]
enum NetherNetHttpType {
    Ping,
    Offer,
}

struct NetherNetHttpRequest;

// http messages coming: client -> tunnel -> agent
impl NetherNetHttpRequest {
    pub fn classify(request: &HttpMessage) -> Option<NetherNetHttpType> {
        let Ok(header) = request.get_request_line() else {
            return None;
        };

        Self::classify_path(header.method, header.path)
    }

    fn classify_path(method: &str, path: &str) -> Option<NetherNetHttpType> {
        /* Rigidly reject anything that is invalid. Accept only the following:
           - POST /v1/join/1371988763283590864
           - GET /v1/join
        */

        match method {
            "GET" if path.strip_prefix("/v1/join").is_some_and(|v| v.is_empty()) => {
                Some(NetherNetHttpType::Ping)
            }
            "POST"
                if path
                    .strip_prefix("/v1/join/")
                    .is_some_and(|v| v.parse::<u64>().is_ok()) =>
            {
                Some(NetherNetHttpType::Offer)
            }
            _ => None,
        }
    }
}

struct NetherNetHttpResponse;

// http messages going: Bedrock server -> agent
impl NetherNetHttpResponse {
    /// A ping is answered with JSON status, an offer with an SDP answer.
    pub fn validate(request_type: &NetherNetHttpType, response: &HttpMessage) -> bool {
        let Ok(status) = response.get_response_status() else {
            return false;
        };
        let content_type = match request_type {
            NetherNetHttpType::Ping => "application/json",
            NetherNetHttpType::Offer => "application/sdp",
        };
        status.is_success() && response.is_content_type(content_type)
    }
}

struct SdpOffer;

impl SdpOffer {
    // We strip out all the candidates from the incoming SDP offer.
    // They contain the client's network address list. If they are present, then if the bedrock server
    // fails to see any STUN packets, the bedrock server would send a STUN directly from each of its network
    // interfaces to each network address listed in this SDP offer. Since the client ignores those anyways,
    // and they reveal the otherwise hidden agent's IP address, we strip these here to prevent that behavior.
    pub fn rewrite(body: &[u8]) -> Vec<u8> {
        let mut new_body = Vec::with_capacity(body.len());
        for line in body.split(|&b| b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if !line.is_empty()
                && !line.starts_with(b"a=candidate:")
                && !line.starts_with(b"a=remote-candidates:")
            {
                new_body.extend_from_slice(line);
                new_body.extend_from_slice(b"\r\n");
            }
        }
        new_body
    }
}

// Read the http request from the client, pass it along to the local service,
// read the local services response, and send that back to the client.
// While performing the required mutations and validations for NetherNet.
async fn proxy_http(
    mut tunn: TcpStream,
    mut origin: TcpStream,
    context: JoinContext,
    sessions: &NetherNetSessions,
    stats: &AgentStats,
) -> Result<JoinOutcome, JoinError> {
    let mut first = [0; 1];
    if tunn
        .peek(&mut first)
        .await
        .map_err(|error| JoinError::Request(HttpError::Io(error)))?
        == 0
    {
        return Err(JoinError::Request(HttpError::Closed));
    }

    if HttpsRejection::looks_like_https(&first) {
        HttpsRejection::write_rejection(&mut tunn, stats)
            .await
            .map_err(JoinError::WriteTunnel)?;
        return Ok(JoinOutcome::TlsRefused);
    }

    /* Receive incoming request. */
    let mut request = HttpMessage::read(&mut tunn, Limits::HEAD, Limits::BODY)
        .await
        .map_err(JoinError::Request)?;

    let Some(request_type) = NetherNetHttpRequest::classify(&request) else {
        return Err(JoinError::Request(HttpError::Invalid));
    };

    if request_type == NetherNetHttpType::Offer {
        request.set_body(SdpOffer::rewrite(&request.body));
    }

    /* Send request to destination application. */
    let request_bytes = request.to_bytes();
    stats.add_bytes_in(request_bytes.len() as u64);
    origin
        .write_all(&request_bytes)
        .await
        .map_err(|error| JoinError::Origin(HttpError::Io(error)))?;

    /* Read response back from application. */
    let mut response = HttpMessage::read(&mut origin, Limits::HEAD, Limits::BODY)
        .await
        .map_err(JoinError::Origin)?;

    if !NetherNetHttpResponse::validate(&request_type, &response) {
        return Err(JoinError::UnexpectedReply);
    }
    let outcome = match request_type {
        NetherNetHttpType::Ping => JoinOutcome::Status,
        NetherNetHttpType::Offer => {
            let rewritten = SdpAnswer::rewrite(&response.body, context.connect_addr)
                .map_err(JoinError::Answer)?;
            let origin_ip = origin
                .peer_addr()
                .map_err(|error| JoinError::Origin(HttpError::Io(error)))?
                .ip();
            let bedrock_addr =
                get_bedrock_udp_addr(origin_ip, rewritten.port, context.connect_addr)
                    .map_err(|error| JoinError::Origin(HttpError::Io(error)))?;

            let stored = sessions.insert(
                now_milli(),
                context.tunnel_id,
                &rewritten.ufrag,
                context.peer_addr.ip(),
                bedrock_addr,
            );
            if !stored {
                return Err(JoinError::TooManyJoins);
            }

            response.set_body(rewritten.body);
            JoinOutcome::Answer
        }
    };

    let reply_bytes = response.to_bytes();
    stats.add_bytes_out(reply_bytes.len() as u64);
    tunn.write_all(&reply_bytes)
        .await
        .map_err(JoinError::WriteTunnel)?;

    let _ = tunn.shutdown().await;
    let _ = origin.shutdown().await;

    Ok(outcome)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    pub fn is_valid_http_path_tests() {
        assert_eq!(
            NetherNetHttpRequest::classify_path("POST", "/v1/join/1371988763283590864"),
            Some(NetherNetHttpType::Offer)
        );
        assert_eq!(
            NetherNetHttpRequest::classify_path("GET", "/v1/join"),
            Some(NetherNetHttpType::Ping)
        );

        assert_eq!(
            NetherNetHttpRequest::classify_path("POST", "/v1/join/"),
            None
        );
        assert_eq!(
            NetherNetHttpRequest::classify_path("GETFOO", "/v1/join"),
            None
        );
        assert_eq!(
            NetherNetHttpRequest::classify_path("GET", "/v1/join/FOO"),
            None
        );
        assert_eq!(
            NetherNetHttpRequest::classify_path("POST", "/v1/join/1371988763283590864/extra"),
            None
        );
        assert_eq!(
            NetherNetHttpRequest::classify_path("POST", "/v1/join/FOO"),
            None
        );
        assert_eq!(
            NetherNetHttpRequest::classify_path("POST", "/v1/join"),
            None
        );
    }
}
