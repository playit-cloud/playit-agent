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
use crate::network::udp::udp_clients::PacketAction;

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

    /// `current` is the client already at this address, as its NetherNet state
    /// (None for a RakNet client) and the Bedrock server UDP address its packets
    /// are sent to. The whole thing is None when the agent's own UDP socket for
    /// that client has closed.
    pub fn route(
        packet: &[u8],
        tunnel_id: u64,
        client_ip: IpAddr,
        now_ms: u64,
        sessions: &NetherNetSessions,
        current: Option<(Option<&NetherNetUdpState>, SocketAddr)>,
    ) -> PacketAction {
        let Some(ufrag) = Stun::bedrock_ufrag(packet) else {
            /* Not STUN, so RakNet or gameplay. A RakNet client shares the port
            and gets an ordinary flow to the configured port. */
            return match current {
                None => PacketAction::Open { join: None },
                Some((None, _)) => PacketAction::Forward,
                Some((Some(state), _)) if state.is_established => PacketAction::Forward,
                Some((Some(_), _)) => PacketAction::Drop,
            };
        };
        let join = sessions.bedrock_addr(now_ms, tunnel_id, ufrag, client_ip);

        /* A retransmit, or the same client's other interface. An established
        flow keeps taking these after its join has left the session table. */
        if let Some((Some(flow), target_addr)) = current
            && flow.ufrag == ufrag
            && !flow.is_expired(now_ms)
            && join.is_none_or(|addr| addr == target_addr)
        {
            return PacketAction::Forward;
        }

        match join {
            Some(target_addr) => PacketAction::Open {
                join: Some((
                    NetherNetUdpState {
                        ufrag: ufrag.to_vec(),
                        opened_at_ms: now_ms,
                        is_established: false,
                    },
                    target_addr,
                )),
            },
            None => PacketAction::Drop,
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
        current: Option<(Option<&NetherNetUdpState>, SocketAddr)>,
    ) -> PacketAction {
        NetherNetUdpState::route(packet, TUNNEL, client_ip, now_ms, sessions, current)
    }

    fn opens_toward(route: PacketAction, target_addr: SocketAddr) -> NetherNetUdpState {
        match route {
            PacketAction::Open {
                join: Some((flow, opened)),
            } if opened == target_addr => flow,
            PacketAction::Open {
                join: Some((_, opened)),
            } => panic!("opened toward {opened}"),
            PacketAction::Open { join: None } => panic!("opened without a join"),
            PacketAction::Forward => panic!("forwarded instead of opening"),
            PacketAction::Drop => panic!("dropped instead of opening"),
        }
    }

    #[test]
    fn a_flow_opens_on_a_join_and_carries_gameplay_once_bedrock_accepts() {
        let sessions = NetherNetSessions::new();
        assert!(sessions.insert(1_000, TUNNEL, b"rJb7", CLIENT, BEDROCK));
        let request = binding_request(b"rJb7:SWk/");

        /* a non-STUN packet with no flow is a RakNet client sharing the port,
        and its later packets keep using that flow */
        assert!(matches!(
            route(&GAMEPLAY, CLIENT, 1_000, &sessions, None),
            PacketAction::Open { join: None }
        ));
        assert!(matches!(
            route(&GAMEPLAY, CLIENT, 1_000, &sessions, Some((None, BEDROCK))),
            PacketAction::Forward
        ));
        let mut flow = opens_toward(route(&request, CLIENT, 1_000, &sessions, None), BEDROCK);
        let current = Some((Some(&flow), BEDROCK));

        /* pending: repeats of the request pass, gameplay does not */
        assert!(matches!(
            route(&request, CLIENT, 1_000, &sessions, current),
            PacketAction::Forward
        ));
        assert!(matches!(
            route(&GAMEPLAY, CLIENT, 1_000, &sessions, current),
            PacketAction::Drop
        ));

        /* an unknown ufrag, or the right ufrag from another client, is dropped */
        let stranger = binding_request(b"Qs0z:SWk/");
        assert!(matches!(
            route(&stranger, CLIENT, 1_000, &sessions, current),
            PacketAction::Drop
        ));
        let other_ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 2));
        assert!(matches!(
            route(&request, other_ip, 1_000, &sessions, None),
            PacketAction::Drop
        ));

        /* the Bedrock server echoing a request back is not acceptance, a success is */
        flow.establish(&request);
        assert!(matches!(
            route(
                &GAMEPLAY,
                CLIENT,
                1_000,
                &sessions,
                Some((Some(&flow), BEDROCK))
            ),
            PacketAction::Drop
        ));
        flow.establish(&BINDING_SUCCESS);
        assert!(matches!(
            route(
                &GAMEPLAY,
                CLIENT,
                1_000,
                &sessions,
                Some((Some(&flow), BEDROCK))
            ),
            PacketAction::Forward
        ));

        /* established: the join is gone, but the client's other interface still gets in */
        let later = 1_000 + NetherNetSessions::LIFETIME_MS + 1;
        assert!(!flow.is_expired(later));
        assert!(matches!(
            route(
                &request,
                CLIENT,
                later,
                &sessions,
                Some((Some(&flow), BEDROCK))
            ),
            PacketAction::Forward
        ));
        assert!(matches!(
            route(&request, CLIENT, later, &sessions, None),
            PacketAction::Drop
        ));

        /* a fresh join toward another Bedrock server socket replaces the flow */
        let other_socket = SocketAddr::new(BEDROCK.ip(), 19141);
        assert!(sessions.insert(later, TUNNEL, b"rJb7", CLIENT, other_socket));
        opens_toward(
            route(
                &request,
                CLIENT,
                later,
                &sessions,
                Some((Some(&flow), BEDROCK)),
            ),
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
                Some((Some(&flow), BEDROCK)),
            ),
            BEDROCK,
        );
    }
}
