//! Pending joins map an ICE ufrag to the Bedrock server socket the client's
//! STUN packets must be forwarded to. The server verifies the ICE password itself.
//!
//! Entries are created when the sdp/http answer is sent.
//! Read when a stun/udp arrives to find the local routing back to the Bedrock server's udp address.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct SessionKey {
    tunnel_id: u64,
    ufrag: Vec<u8>,
}

struct Session {
    bedrock_addr: SocketAddr,
    /// Public source of the TCP join. STUN packets must come from the same client IP.
    client_ip: IpAddr,
    expires_ms: u64,
}

pub struct NetherNetSessions {
    entries: Mutex<HashMap<SessionKey, Session>>,
}

impl Default for NetherNetSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl NetherNetSessions {
    // Lifetime between sdp/http answer sent and when the stun/udp must arrive.
    pub const LIFETIME_MS: u64 = 10_000;
    /// Pending joins across all tunnels. A full table refuses new joins.
    const CAPACITY: usize = 4096;

    pub fn new() -> Self {
        NetherNetSessions {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Remembers where a client's STUN packets go once its answer is sent.
    /// Returns false when the table is full of joins that have not expired.
    pub fn insert(
        &self,
        now_ms: u64,
        tunnel_id: u64,
        ufrag: &[u8],
        client_ip: IpAddr,
        bedrock_addr: SocketAddr,
    ) -> bool {
        let key = SessionKey {
            tunnel_id,
            ufrag: ufrag.to_vec(),
        };
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= Self::CAPACITY && !entries.contains_key(&key) {
            entries.retain(|_, session| now_ms <= session.expires_ms);
            if entries.len() >= Self::CAPACITY {
                return false;
            }
        }
        entries.insert(
            key,
            Session {
                bedrock_addr,
                client_ip: client_ip.to_canonical(),
                expires_ms: now_ms.saturating_add(Self::LIFETIME_MS),
            },
        );
        true
    }

    /// The Bedrock server socket for a STUN packet carrying `ufrag`, sent from
    /// `client_ip`. The entry stays until it expires: a client with several
    /// interfaces sends a STUN packet from each one.
    pub fn bedrock_addr(
        &self,
        now_ms: u64,
        tunnel_id: u64,
        ufrag: &[u8],
        client_ip: IpAddr,
    ) -> Option<SocketAddr> {
        let key = SessionKey {
            tunnel_id,
            ufrag: ufrag.to_vec(),
        };
        let entries = self.entries.lock().unwrap();
        let session = entries.get(&key)?;
        if session.expires_ms < now_ms || session.client_ip != client_ip.to_canonical() {
            return None;
        }
        Some(session.bedrock_addr)
    }
}

#[cfg(test)]
mod test {
    use std::net::Ipv4Addr;

    use super::*;

    const CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
    const OTHER: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 4));
    const BEDROCK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 19140);
    const OTHER_BEDROCK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 19141);

    #[test]
    fn a_join_is_found_by_tunnel_ufrag_and_client_ip_until_it_expires() {
        let sessions = NetherNetSessions::new();
        let now = 1_000;
        assert_eq!(sessions.bedrock_addr(now, 7, b"rJb7", CLIENT), None);

        assert!(sessions.insert(now, 7, b"rJb7", CLIENT, BEDROCK));
        assert_eq!(sessions.bedrock_addr(now, 8, b"rJb7", CLIENT), None);
        assert_eq!(sessions.bedrock_addr(now, 7, b"rJb7", OTHER), None);
        assert_eq!(
            sessions.bedrock_addr(now, 7, b"rJb7", CLIENT),
            Some(BEDROCK)
        );
        assert_eq!(
            sessions.bedrock_addr(now + NetherNetSessions::LIFETIME_MS, 7, b"rJb7", CLIENT),
            Some(BEDROCK)
        );
        assert_eq!(
            sessions.bedrock_addr(now + NetherNetSessions::LIFETIME_MS + 1, 7, b"rJb7", CLIENT),
            None
        );

        /* a fresh join for the same ufrag points at the new socket */
        assert!(sessions.insert(now, 7, b"rJb7", CLIENT, OTHER_BEDROCK));
        assert_eq!(
            sessions.bedrock_addr(now, 7, b"rJb7", CLIENT),
            Some(OTHER_BEDROCK)
        );
    }

    #[test]
    fn a_full_table_refuses_joins_until_old_ones_expire() {
        let sessions = NetherNetSessions::new();
        for i in 0..NetherNetSessions::CAPACITY {
            assert!(sessions.insert(0, 1, format!("u{i}").as_bytes(), CLIENT, BEDROCK));
        }

        /* full and nothing expired: a repeat join fits, a new one does not */
        assert!(sessions.insert(1, 1, b"u0", CLIENT, OTHER_BEDROCK));
        assert!(!sessions.insert(1, 1, b"new", CLIENT, BEDROCK));
        assert_eq!(sessions.bedrock_addr(1, 1, b"new", CLIENT), None);

        /* once the old joins expire, they make room */
        let later = NetherNetSessions::LIFETIME_MS + 2;
        assert!(sessions.insert(later, 1, b"new", CLIENT, BEDROCK));
        assert_eq!(
            sessions.bedrock_addr(later, 1, b"new", CLIENT),
            Some(BEDROCK)
        );
        assert_eq!(sessions.bedrock_addr(later, 1, b"u1", CLIENT), None);
    }
}
