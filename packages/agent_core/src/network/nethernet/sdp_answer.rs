//! Replaces the answer's candidate list and connection addresses with the public tunnel endpoint.
//! The Bedrock server's UDP port and ICE ufrag stay in the agent for UDP routing.

use std::net::SocketAddr;

use playit_common::VecExt;

use super::stun::Stun;

pub struct RewrittenAnswer {
    pub body: Vec<u8>,
    // The "user" identifier that comes in over the sdp/http and the assocation of stun/udp packets
    // back, required for proper single->multi port muxing.
    pub ufrag: Vec<u8>,
    /// The UDP port the Bedrock server allocated for this session, bound on each of its interfaces.
    pub port: u16,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SdpAnswerError {
    /// The media line carries no real port. The Bedrock server answers this way when it
    /// could not allocate a UDP socket for the session.
    NoDestinationPort,
    NoUfrag,
    Malformed,
}

pub struct SdpAnswer;

impl SdpAnswer {
    const CANDIDATE: &[u8] = b"a=candidate:";
    const ICE_UFRAG: &[u8] = b"a=ice-ufrag:";
    const CONNECTION: &[u8] = b"c=";
    const ORIGIN: &[u8] = b"o=";
    const MEDIA: &[u8] = b"m=";

    /// WebRTC writes the discard port on the media line when it gathered no candidates.
    const DISCARD_PORT: u16 = 9;

    /// Rewrites the Bedrock server's SDP answer so the client sees only the public tunnel
    /// address, and reads out what the agent needs to route the session's UDP.
    pub fn rewrite(body: &[u8], public: SocketAddr) -> Result<RewrittenAnswer, SdpAnswerError> {
        let destination_port = Self::find_destination_port(body)?;
        let ip = public.ip().to_string();
        let addrtype: &[u8] = if public.is_ipv4() { b"IP4" } else { b"IP6" };
        let port = public.port().to_string();

        let mut out = Vec::with_capacity(body.len());
        let mut ufrag = None;
        let mut candidate_written = false;

        for line in Self::lines(body) {
            if line.starts_with(Self::CANDIDATE) {
                if !candidate_written {
                    Self::push_candidate(&mut out, ip.as_bytes(), port.as_bytes());
                    candidate_written = true;
                }
                continue;
            }

            if let Some(rest) = line.strip_prefix(Self::ICE_UFRAG) {
                if rest.is_empty() || rest.len() > Stun::MAX_UFRAG_LEN {
                    return Err(SdpAnswerError::NoUfrag);
                }
                ufrag = Some(rest.to_vec());
            } else if line.starts_with(Self::CONNECTION) {
                /* With ICE the real connection candidates are the `a=candidate:` lines.
                 * Only pre-ICE legacy SDP uses c= but it's preserved (via rewrite w/ tunnel's address) anyways. */
                out.extend_from_slice(Self::CONNECTION);
                out.extend_from_slices_with_separator(&[b"IN", addrtype, ip.as_bytes()], b" ");
                out.extend_from_slice(b"\r\n");
                continue;
            } else if line.starts_with(Self::ORIGIN) {
                /* The session id and version only matter across renegotiations. */
                out.extend_from_slice(Self::ORIGIN);
                out.extend_from_slices_with_separator(
                    &[b"-", b"0", b"0", b"IN", addrtype, ip.as_bytes()],
                    b" ",
                );
                out.extend_from_slice(b"\r\n");
                continue;
            } else if let Some(rest) = line.strip_prefix(Self::MEDIA) {
                /* m=<media> <port> <proto> <fmt>
                 * This is part of pre-ICE legacy SDP but Bedrock wont connect without it. */
                let mut fields = rest.split(|&b| b == b' ');
                if let (Some(media), Some(_port)) = (fields.next(), fields.next()) {
                    out.extend_from_slice(Self::MEDIA);
                    out.extend_from_slice(media);
                    out.push(b' ');
                    out.extend_from_slice(port.as_bytes());
                    for field in fields {
                        out.push(b' ');
                        out.extend_from_slice(field);
                    }
                    out.extend_from_slice(b"\r\n");
                    continue;
                }
            }

            if line.is_empty() {
                continue;
            }
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
        }

        /* The client needs one candidate even when the Bedrock server listed none. */
        if !candidate_written {
            Self::push_candidate(&mut out, ip.as_bytes(), port.as_bytes());
        }

        Ok(RewrittenAnswer {
            body: out,
            ufrag: ufrag.ok_or(SdpAnswerError::NoUfrag)?,
            port: destination_port,
        })
    }

    /// The UDP port the Bedrock server allocated for this session, from the
    /// media line. The server binds it on every interface it advertises, so the
    /// address the agent already reaches the server at is enough. Candidate addresses are
    /// never used as a destination.
    fn find_destination_port(body: &[u8]) -> Result<u16, SdpAnswerError> {
        let media = Self::lines(body)
            .find_map(|line| line.strip_prefix(Self::MEDIA))
            .ok_or(SdpAnswerError::Malformed)?;
        /* m=<media> <port> <proto> <fmt> */
        let port = media
            .split(|&b| b == b' ')
            .nth(1)
            .and_then(|field| std::str::from_utf8(field).ok())
            .and_then(|field| field.parse::<u16>().ok())
            .ok_or(SdpAnswerError::Malformed)?;
        match port {
            0 | Self::DISCARD_PORT => Err(SdpAnswerError::NoDestinationPort),
            port => Ok(port),
        }
    }

    fn push_candidate(out: &mut Vec<u8>, ip: &[u8], port: &[u8]) {
        out.extend_from_slice(Self::CANDIDATE);
        out.extend_from_slices_with_separator(
            &[
                b"1",
                b"1",
                b"udp",
                b"2130706431",
                ip,
                port,
                b"typ",
                b"host",
                b"generation",
                b"0",
            ],
            b" ",
        );
        out.extend_from_slice(b"\r\n");
    }

    /// SDP lines, with either line ending accepted.
    fn lines(body: &[u8]) -> impl Iterator<Item = &[u8]> {
        body.split(|&b| b == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
    }
}

#[cfg(test)]
mod test {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::*;

    /* the captured answer with the identity token cut down */
    const ANSWER: &str = "v=0\r\n\
o=- 7033223701722840923 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0\r\n\
a=extmap-allow-mixed\r\n\
a=msid-semantic: WMS\r\n\
a=identity:eyJhc3NlcnRpb24iOiJ7XCJmaW5nZXJwcmludHNcIjpcImV5SmhiR2NpT2lKRlV6TTROQ0o5\r\n\
m=application 19140 UDP/DTLS/SCTP webrtc-datachannel\r\n\
c=IN IP4 172.19.0.1\r\n\
a=candidate:2210547580 1 udp 2122194687 172.19.0.1 19140 typ host generation 0 network-id 1 network-cost 50\r\n\
a=candidate:4082157980 1 udp 2122129151 10.250.100.1 19140 typ host generation 0 network-id 5 network-cost 50\r\n\
a=candidate:3503833759 1 udp 2122063615 192.168.1.11 19140 typ host generation 0 network-id 2\r\n\
a=candidate:1744561467 1 udp 2122265343 fd46:6:100::1 19140 typ host generation 0 network-id 6 network-cost 50\r\n\
a=ice-ufrag:rJb7\r\n\
a=ice-pwd:g8KKcbTOpKSUknrc38+WbYr5\r\n\
a=ice-options:trickle\r\n\
a=fingerprint:sha-256 BB:FE:B7:D2:1B:FC:8F:BC:36:A7:81:95:FE:93:90:C1:41:EE:FD:33:19:F3:A5:22:7A:E8:8C:49:5F:65:9D:A0\r\n\
a=setup:active\r\n\
a=mid:0\r\n\
a=sctp-port:5000\r\n\
a=max-message-size:262144\r\n";

    const PUBLIC: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(147, 185, 221, 2)), 30123);

    const PUBLIC_CANDIDATE: &str =
        "a=candidate:1 1 udp 2130706431 147.185.221.2 30123 typ host generation 0";

    fn lines(body: &[u8]) -> Vec<&str> {
        std::str::from_utf8(body)
            .unwrap()
            .split("\r\n")
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn candidates(body: &[u8]) -> Vec<&str> {
        lines(body)
            .into_iter()
            .filter(|l| l.starts_with("a=candidate:"))
            .collect()
    }

    fn without_lines(answer: &str, prefix: &str) -> String {
        answer
            .lines()
            .filter(|l| !l.starts_with(prefix))
            .map(|l| format!("{l}\r\n"))
            .collect()
    }

    #[test]
    fn answer_points_at_the_public_address_and_nothing_else() {
        let rewritten = SdpAnswer::rewrite(ANSWER.as_bytes(), PUBLIC).unwrap();
        assert_eq!(rewritten.ufrag, b"rJb7");
        assert_eq!(rewritten.port, 19140);

        let out = lines(&rewritten.body);
        assert_eq!(candidates(&rewritten.body), vec![PUBLIC_CANDIDATE]);
        assert!(out.contains(&"c=IN IP4 147.185.221.2"));
        assert!(out.contains(&"o=- 0 0 IN IP4 147.185.221.2"));
        assert!(out.contains(&"m=application 30123 UDP/DTLS/SCTP webrtc-datachannel"));

        /* every server address and the server port are gone */
        let text = std::str::from_utf8(&rewritten.body).unwrap();
        for leak in [
            "172.19.0.1",
            "10.250.100.1",
            "192.168.1.11",
            "fd46:6:100::1",
            "127.0.0.1",
            "19140",
        ] {
            assert!(!text.contains(leak), "{leak} still in answer");
        }

        /* the signed lines are untouched */
        for kept in [
            "a=ice-ufrag:rJb7",
            "a=ice-pwd:g8KKcbTOpKSUknrc38+WbYr5",
            "a=setup:active",
            "a=sctp-port:5000",
        ] {
            assert!(out.contains(&kept), "{kept} missing");
        }
        assert!(
            text.starts_with("a=identity:") || text.contains("\r\na=identity:eyJhc3NlcnRpb24i")
        );
    }

    /// The Bedrock server's `server-public-ip` setting turns every candidate into a
    /// reflexive one naming that address. The port on the media line is still
    /// the socket the server bound, and the configured address must not leak.
    #[test]
    fn port_comes_from_the_media_line_not_the_candidates() {
        let answer = ANSWER
            .replace("c=IN IP4 172.19.0.1", "c=IN IP4 1.1.1.1")
            .replace(
                "a=candidate:2210547580 1 udp 2122194687 172.19.0.1 19140 typ host",
                "a=candidate:2105128546 1 udp 1685987071 1.1.1.1 19140 typ srflx raddr 172.19.0.1 rport 19140",
            );
        let rewritten = SdpAnswer::rewrite(answer.as_bytes(), PUBLIC).unwrap();
        assert_eq!(rewritten.port, 19140);
        assert_eq!(candidates(&rewritten.body), vec![PUBLIC_CANDIDATE]);
        let text = std::str::from_utf8(&rewritten.body).unwrap();
        assert!(!text.contains("1.1.1.1"));
        assert!(!text.contains("172.19.0.1"));
    }

    #[test]
    fn answer_without_candidates_still_offers_the_public_one() {
        let answer = without_lines(ANSWER, "a=candidate:");
        let rewritten = SdpAnswer::rewrite(answer.as_bytes(), PUBLIC).unwrap();
        assert_eq!(rewritten.port, 19140);
        assert_eq!(candidates(&rewritten.body), vec![PUBLIC_CANDIDATE]);
    }

    #[test]
    fn public_ipv6_is_written_as_ip6() {
        let public = SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2602, 0xfbaf, 0, 0, 0, 0, 0, 0x2)),
            30123,
        );
        let rewritten = SdpAnswer::rewrite(ANSWER.as_bytes(), public).unwrap();
        let out = lines(&rewritten.body);
        assert!(out.contains(&"c=IN IP6 2602:fbaf::2"));
        assert_eq!(
            candidates(&rewritten.body),
            vec!["a=candidate:1 1 udp 2130706431 2602:fbaf::2 30123 typ host generation 0"]
        );
    }

    #[test]
    fn answers_missing_what_the_agent_needs_are_refused() {
        /* The Bedrock server could not allocate a socket: no candidates and the discard port. */
        let unallocated =
            without_lines(ANSWER, "a=candidate:").replace("m=application 19140", "m=application 9");
        assert_eq!(
            SdpAnswer::rewrite(unallocated.as_bytes(), PUBLIC).err(),
            Some(SdpAnswerError::NoDestinationPort)
        );

        let no_media = without_lines(ANSWER, "m=");
        assert_eq!(
            SdpAnswer::rewrite(no_media.as_bytes(), PUBLIC).err(),
            Some(SdpAnswerError::Malformed)
        );

        let no_ufrag = without_lines(ANSWER, "a=ice-ufrag:");
        assert_eq!(
            SdpAnswer::rewrite(no_ufrag.as_bytes(), PUBLIC).err(),
            Some(SdpAnswerError::NoUfrag)
        );
    }
}
