//! Buffers one NetherNet signaling message so SDP can be edited before forwarding.

use playit_common::ByteSliceExt;
use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Debug)]
pub enum HttpError {
    Io(std::io::Error),
    Closed,
    TooLarge,
    /// Not the plain, fixed-length HTTP/1.1 that the Bedrock server and client speak.
    Invalid,
}

impl From<std::io::Error> for HttpError {
    fn from(error: std::io::Error) -> Self {
        HttpError::Io(error)
    }
}

pub struct HttpRequestLine<'a> {
    pub method: &'a str,
    pub path: &'a str,
}

impl<'a> HttpRequestLine<'a> {
    fn parse(line: &'a str) -> Result<Self, HttpError> {
        // GET /v1/join HTTP/1.1
        let mut parts = line.split(' ');
        let method = parts.next().ok_or(HttpError::Invalid)?;
        let path = parts.next().ok_or(HttpError::Invalid)?;
        Ok(HttpRequestLine { method, path })
    }
}

type Headers = Vec<(String, String)>;

pub struct HttpResponseStatus {
    pub status_code: u16,
}

impl HttpResponseStatus {
    pub fn parse(line: &str) -> Result<Self, HttpError> {
        // HTTP/1.1 200 OK
        let status_code = line
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or(HttpError::Invalid)?;
        Ok(HttpResponseStatus { status_code })
    }

    pub fn is_success(&self) -> bool {
        self.status_code == 200
    }
}

pub struct HttpMessage {
    start_line: String,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl HttpMessage {
    const LINE_END: &'static str = "\r\n";
    const CONTENT_LENGTH: &'static str = "content-length";

    pub async fn read<R: AsyncRead + Unpin>(
        reader: &mut R,
        head_limit: usize,
        body_limit: usize,
    ) -> Result<Self, HttpError> {
        /* the blank line that ends the head */
        const HEAD_END: &[u8] = b"\r\n\r\n";

        let mut buffer = Vec::with_capacity(1024);
        let mut chunk = [0u8; 1024];

        let head_len = loop {
            if let Some(pos) = buffer.find(HEAD_END) {
                let len = pos + HEAD_END.len();
                if len > head_limit {
                    return Err(HttpError::TooLarge);
                }
                break len;
            }
            if buffer.len() >= head_limit {
                return Err(HttpError::TooLarge);
            }

            let read = reader.read(&mut chunk).await?;
            if read == 0 {
                return Err(HttpError::Closed);
            }
            buffer.extend_from_slice(&chunk[..read]);
        };

        let head = std::str::from_utf8(&buffer[..head_len]).map_err(|_| HttpError::Invalid)?;
        let (start_line, headers) = Self::parse_head(head)?;
        let message = HttpMessage {
            start_line: start_line.to_owned(),
            headers,
            body: Vec::new(),
        };

        /* Only fixed-length bodies are framed. Chunked encoding never appears in signaling. */
        if message.header("transfer-encoding").is_some() {
            return Err(HttpError::Invalid);
        }
        let content_length = match message.header(Self::CONTENT_LENGTH) {
            Some(value) => value.parse::<usize>().map_err(|_| HttpError::Invalid)?,
            None => 0,
        };
        if content_length > body_limit {
            return Err(HttpError::TooLarge);
        }

        let mut body = buffer[head_len..].to_vec();
        body.truncate(content_length);
        while body.len() < content_length {
            let want = (content_length - body.len()).min(chunk.len());
            let read = reader.read(&mut chunk[..want]).await?;
            if read == 0 {
                return Err(HttpError::Closed);
            }
            body.extend_from_slice(&chunk[..read]);
        }

        Ok(HttpMessage { body, ..message })
    }

    pub fn get_request_line(&self) -> Result<HttpRequestLine<'_>, HttpError> {
        HttpRequestLine::parse(&self.start_line)
    }

    pub fn get_response_status(&self) -> Result<HttpResponseStatus, HttpError> {
        HttpResponseStatus::parse(&self.start_line)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn is_content_type(&self, content_type: &str) -> bool {
        self.header("content-type")
            .is_some_and(|v| v.eq_ignore_ascii_case(content_type))
    }

    /// Replaces the body and the Content-Length header with it.
    pub fn set_body(&mut self, body: Vec<u8>) {
        let len = body.len().to_string();
        match self
            .headers
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(Self::CONTENT_LENGTH))
        {
            Some((_, value)) => *value = len,
            None => self.headers.push((Self::CONTENT_LENGTH.to_owned(), len)),
        }
        self.body = body;
    }

    // Returns http message as bytes for the wire.
    pub fn to_bytes(&self) -> Vec<u8> {
        const SEPARATOR: &[u8] = b": ";
        let line_end = Self::LINE_END.as_bytes();

        // Rough allocation, give 64 bytes per header.
        let headers_len = self.start_line.len() + self.body.len() + self.headers.len() * 64;
        let mut out = Vec::with_capacity(
            self.start_line.len() + line_end.len() + headers_len + line_end.len() + self.body.len(),
        );
        out.extend_from_slice(self.start_line.as_bytes());
        out.extend_from_slice(line_end);
        for (name, value) in &self.headers {
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(SEPARATOR);
            out.extend_from_slice(value.as_bytes());
            out.extend_from_slice(line_end);
        }
        out.extend_from_slice(line_end);
        out.extend_from_slice(&self.body);
        out
    }

    /// Splits the head into its start line and `name: value` headers.
    fn parse_head(head: &str) -> Result<(&str, Headers), HttpError> {
        let mut lines = head.split(Self::LINE_END);
        let start_line = lines
            .next()
            .filter(|l| !l.is_empty())
            .ok_or(HttpError::Invalid)?;

        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            let (name, value) = line.split_once(':').ok_or(HttpError::Invalid)?;
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
        Ok((start_line, headers))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn reads_a_message_across_reads_and_rewrites_its_length() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/sdp\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".to_vec();
        let mut reader = tokio_test_reader(raw.clone(), 7);

        let mut message = HttpMessage::read(&mut reader, 1024, 1024).await.unwrap();
        assert_eq!(message.get_response_status().unwrap().status_code, 200);
        assert_eq!(message.header("content-type"), Some("application/sdp"));
        assert_eq!(message.body, b"hello");
        assert_eq!(message.to_bytes(), raw);

        message.set_body(b"hello world".to_vec());
        assert_eq!(message.header("Content-Length"), Some("11"));
        assert!(message.to_bytes().ends_with(b"\r\n\r\nhello world"));

        /* a reason phrase with spaces is still a status line */
        let status = HttpResponseStatus::parse("HTTP/1.1 404 Not Found").unwrap();
        assert_eq!(status.status_code, 404);
        assert!(!status.is_success());
    }

    async fn expect_err(str: &[u8], expected: HttpError) {
        let mut reader = tokio_test_reader(str.to_vec(), 64);
        let actual = HttpMessage::read(&mut reader, 1024, 8192)
            .await
            .err()
            .expect("expected an error");
        assert_eq!(
            std::mem::discriminant(&actual),
            std::mem::discriminant(&expected),
        );
    }

    #[tokio::test]
    async fn limits_and_framing_are_enforced() {
        expect_err(
            b"POST /v1/join/1 HTTP/1.1\r\nContent-Length: 9000\r\n\r\n",
            HttpError::TooLarge,
        )
        .await;
        expect_err(
            &format!(
                "GET /v1/join HTTP/1.1\r\nX-Padding: {}\r\n\r\n",
                "a".repeat(1100)
            )
            .into_bytes(),
            HttpError::TooLarge,
        )
        .await;
        expect_err(&[b'a'; 2000], HttpError::TooLarge).await;

        expect_err(
            b"GET /v1/join HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            HttpError::Invalid,
        )
        .await;
        expect_err(
            b"GET /v1/join HTTP/1.1\r\nContent-Length: many\r\n\r\n",
            HttpError::Invalid,
        )
        .await;

        expect_err(
            b"GET /v1/join HTTP/1.1\r\nContent-Length: 4\r\n\r\nab",
            HttpError::Closed,
        )
        .await;
    }

    /// Serves `data` in reads of at most `chunk` bytes, so head and body
    /// boundaries fall mid-read.
    fn tokio_test_reader(data: Vec<u8>, chunk: usize) -> impl AsyncRead + Unpin {
        struct Chunked {
            data: Vec<u8>,
            pos: usize,
            chunk: usize,
        }
        impl AsyncRead for Chunked {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                let remaining = &self.data[self.pos..];
                let n = remaining.len().min(self.chunk).min(buf.remaining());
                buf.put_slice(&remaining[..n]);
                self.pos += n;
                std::task::Poll::Ready(Ok(()))
            }
        }
        Chunked {
            data,
            pos: 0,
            chunk,
        }
    }
}
