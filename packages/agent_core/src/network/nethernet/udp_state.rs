//! The UDP side of one NetherNet client. Created when client's first stun/udp arrives.
//!
//! A flow has two states:
//! - Pending: opened by the client's first STUN Binding request. Only STUN
//!   is forwarded, and the flow expires if the Bedrock server does not answer in time.
//! - Established: the Bedrock server replied with a Binding success, which is its way
//!   of saying the client passed the ICE password check. All traffic is
//!   forwarded from then on.
//!
//! The agent never checks the ICE password itself. The Bedrock server does.

use std::net::{IpAddr, SocketAddr};

use super::sessions::NetherNetSessions;
use super::stun::Stun;

/// What to do with a packet from the tunnel.
pub enum Route {
    /// Hand it to the client already at this address.
    Forward,
    /// Replace whatever is at this client address. A NetherNet join names the
    /// flow and the Bedrock server socket it forwards to. Ordinary origins carry no
    /// join and resolve their target once the new client is allowed.
    Open {
        join: Option<(NetherNetUdpState, SocketAddr)>,
    },
    Drop,
}

pub struct NetherNetUdpState {
    ufrag: Vec<u8>,
    opened_at_ms: u64,
    /// The Bedrock server's Binding success stun/udp has arrived, allow nethernet/udp through.
    is_established: bool,
}

impl NetherNetUdpState {
    pub fn is_expired(&self, now_ms: u64) -> bool {
        !self.is_established
            && now_ms.saturating_sub(self.opened_at_ms) > NetherNetSessions::LIFETIME_MS
    }

    /// The Bedrock server's Binding success is the agent's only sign that it
    /// verified the client. It ends the STUN-only period and the deadline.
    pub fn establish(&mut self, packet: &[u8]) {
        if Stun::is_binding_success(packet) {
            self.is_established = true;
        }
    }

    /// `current` is the state already kept for this client address, paired
    /// with the Bedrock server UDP address its packets are sent to. It is None
    /// when the agent's own UDP socket for that client has closed.
    pub fn route(
        packet: &[u8],
        tunnel_id: u64,
        client_ip: IpAddr,
        now_ms: u64,
        sessions: &NetherNetSessions,
        current: Option<(&NetherNetUdpState, SocketAddr)>,
    ) -> Route {
        let Some(ufrag) = Stun::bedrock_ufrag(packet) else {
            /* Gameplay may only follow a flow the Bedrock server has accepted. */
            return match current {
                Some((flow, _)) if flow.is_established => Route::Forward,
                _ => Route::Drop,
            };
        };
        let join = sessions.bedrock_addr(now_ms, tunnel_id, ufrag, client_ip);

        /* A retransmit, or the same client's other interface. An established
        flow keeps taking these after its join has left the session table. */
        if let Some((flow, target_addr)) = current
            && flow.ufrag == ufrag
            && !flow.is_expired(now_ms)
            && join.is_none_or(|addr| addr == target_addr)
        {
            return Route::Forward;
        }

        match join {
            Some(target_addr) => Route::Open {
                join: Some((
                    NetherNetUdpState {
                        ufrag: ufrag.to_vec(),
                        opened_at_ms: now_ms,
                        is_established: false,
                    },
                    target_addr,
                )),
            },
            None => Route::Drop,
        }
    }
}

#[cfg(test)]
mod test {
    use std::net::Ipv4Addr;

    use super::*;

    const TUNNEL: u64 = 7;
    const CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
    const BEDROCK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 19140);
    const GAMEPLAY: [u8; 5] = [0x16, 0xfe, 0xfd, 0, 0];
    const BINDING_SUCCESS: [u8; 20] = [
        0x01, 0x01, 0, 0, 0x21, 0x12, 0xA4, 0x42, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    ];

    /// A Binding request carrying one USERNAME attribute.
    fn binding_request(username: &[u8]) -> Vec<u8> {
        let padded = username.len().div_ceil(4) * 4;
        let mut packet = vec![0x00, 0x01];
        packet.extend_from_slice(&((4 + padded) as u16).to_be_bytes());
        packet.extend_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        packet.extend_from_slice(&[7; 12]);
        packet.extend_from_slice(&[0x00, 0x06]);
        packet.extend_from_slice(&(username.len() as u16).to_be_bytes());
        packet.extend_from_slice(username);
        packet.resize(packet.len() + padded - username.len(), 0);
        packet
    }

    fn route(
        packet: &[u8],
        client_ip: IpAddr,
        now_ms: u64,
        sessions: &NetherNetSessions,
        current: Option<(&NetherNetUdpState, SocketAddr)>,
    ) -> Route {
        NetherNetUdpState::route(packet, TUNNEL, client_ip, now_ms, sessions, current)
    }

    fn opens_toward(route: Route, target_addr: SocketAddr) -> NetherNetUdpState {
        match route {
            Route::Open {
                join: Some((flow, opened)),
            } if opened == target_addr => flow,
            Route::Open {
                join: Some((_, opened)),
            } => panic!("opened toward {opened}"),
            Route::Open { join: None } => panic!("opened without a join"),
            Route::Forward => panic!("forwarded instead of opening"),
            Route::Drop => panic!("dropped instead of opening"),
        }
    }

    #[test]
    fn a_flow_opens_on_a_join_and_carries_gameplay_once_bedrock_accepts() {
        let sessions = NetherNetSessions::new();
        assert!(sessions.insert(1_000, TUNNEL, b"rJb7", CLIENT, BEDROCK));
        let request = binding_request(b"rJb7:SWk/");

        /* nothing goes anywhere before a Binding request opens the flow */
        assert!(matches!(
            route(&GAMEPLAY, CLIENT, 1_000, &sessions, None),
            Route::Drop
        ));
        let mut flow = opens_toward(route(&request, CLIENT, 1_000, &sessions, None), BEDROCK);
        let current = Some((&flow, BEDROCK));

        /* pending: repeats of the request pass, gameplay does not */
        assert!(matches!(
            route(&request, CLIENT, 1_000, &sessions, current),
            Route::Forward
        ));
        assert!(matches!(
            route(&GAMEPLAY, CLIENT, 1_000, &sessions, current),
            Route::Drop
        ));

        /* an unknown ufrag, or the right ufrag from another client, is dropped */
        let stranger = binding_request(b"Qs0z:SWk/");
        assert!(matches!(
            route(&stranger, CLIENT, 1_000, &sessions, current),
            Route::Drop
        ));
        let other_ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 2));
        assert!(matches!(
            route(&request, other_ip, 1_000, &sessions, None),
            Route::Drop
        ));

        /* the Bedrock server echoing a request back is not acceptance, a success is */
        flow.establish(&request);
        assert!(matches!(
            route(&GAMEPLAY, CLIENT, 1_000, &sessions, Some((&flow, BEDROCK))),
            Route::Drop
        ));
        flow.establish(&BINDING_SUCCESS);
        assert!(matches!(
            route(&GAMEPLAY, CLIENT, 1_000, &sessions, Some((&flow, BEDROCK))),
            Route::Forward
        ));

        /* established: the join is gone, but the client's other interface still gets in */
        let later = 1_000 + NetherNetSessions::LIFETIME_MS + 1;
        assert!(!flow.is_expired(later));
        assert!(matches!(
            route(&request, CLIENT, later, &sessions, Some((&flow, BEDROCK))),
            Route::Forward
        ));
        assert!(matches!(
            route(&request, CLIENT, later, &sessions, None),
            Route::Drop
        ));

        /* a fresh join toward another Bedrock server socket replaces the flow */
        let other_socket = SocketAddr::new(BEDROCK.ip(), 19141);
        assert!(sessions.insert(later, TUNNEL, b"rJb7", CLIENT, other_socket));
        opens_toward(
            route(&request, CLIENT, later, &sessions, Some((&flow, BEDROCK))),
            other_socket,
        );
    }

    #[test]
    fn a_pending_flow_expires_and_is_replaced_by_a_new_join() {
        let sessions = NetherNetSessions::new();
        assert!(sessions.insert(1_000, TUNNEL, b"rJb7", CLIENT, BEDROCK));
        let request = binding_request(b"rJb7:SWk/");
        let flow = opens_toward(route(&request, CLIENT, 1_000, &sessions, None), BEDROCK);

        assert!(!flow.is_expired(1_000 + NetherNetSessions::LIFETIME_MS));
        let expired_at = 1_001 + NetherNetSessions::LIFETIME_MS;
        assert!(flow.is_expired(expired_at));

        /* the expired flow no longer forwards; a new join opens a new one */
        assert!(sessions.insert(expired_at, TUNNEL, b"rJb7", CLIENT, BEDROCK));
        opens_toward(
            route(
                &request,
                CLIENT,
                expired_at,
                &sessions,
                Some((&flow, BEDROCK)),
            ),
            BEDROCK,
        );
    }
}
