use super::{
    connected_control::ConnectedControl,
    established_control::{EstablishedControl, ExpiredReason},
    *,
};
use message_encoding::MessageEncoding;
use playit_agent_proto::{
    AgentSessionId,
    control_feed::{ClaimInstructions, ControlFeed, NewClient},
    control_messages::{AgentRegistered, ControlResponse, Pong},
    rpc::ControlRpcMessage,
};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Default)]
struct FakeIo(Arc<Mutex<VecDeque<ControlFeed>>>);
impl PacketIO for FakeIo {
    async fn send_to(&self, data: &[u8], _: SocketAddr) -> std::io::Result<usize> {
        Ok(data.len())
    }
    async fn recv_from(&self, out: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        let feed = self.0.lock().unwrap().pop_front();
        if let Some(feed) = feed {
            let count = feed.write_to(&mut &mut out[..])?;
            Ok((count, "127.0.0.1:5525".parse().unwrap()))
        } else {
            std::future::pending().await
        }
    }
}
#[derive(Clone)]
struct Auth;
impl AuthResource for Auth {
    async fn authenticate(&self, _: &Pong) -> Result<SignedAgentKey, errors::SetupError> {
        Ok(SignedAgentKey { key: "00".into() })
    }
    async fn get_control_addresses(&self) -> Result<Vec<SocketAddr>, errors::SetupError> {
        Ok(vec![])
    }
}
fn pong() -> Pong {
    Pong {
        request_now: 10,
        server_now: 1000,
        server_id: 1,
        data_center_id: 1,
        client_addr: "127.0.0.1:1234".parse().unwrap(),
        tunnel_addr: "127.0.0.1:5525".parse().unwrap(),
        session_expire_at: None,
    }
}
fn registered() -> AgentRegistered {
    AgentRegistered {
        id: AgentSessionId {
            session_id: 1,
            account_id: 2,
            agent_id: 3,
        },
        expires_at: 61_000,
    }
}
fn established() -> (EstablishedControl<Auth, FakeIo>, FakeIo) {
    let io = FakeIo::default();
    (
        ConnectedControl::new(pong().tunnel_addr, io.clone(), pong())
            .into_established(Auth, registered()),
        io,
    )
}
fn response(io: &FakeIo, id: u64, content: ControlResponse) {
    io.0.lock()
        .unwrap()
        .push_back(ControlFeed::Response(ControlRpcMessage {
            request_id: id,
            content,
        }));
}

#[tokio::test(start_paused = true)]
async fn successful_registration_is_valid_without_session_in_discovery_pong() {
    let (control, _) = established();
    assert_eq!(control.is_expired(), None);
    tokio::time::advance(Duration::from_secs(61)).await;
    assert_eq!(control.is_expired(), Some(ExpiredReason::Deadline));
}

#[tokio::test(start_paused = true)]
async fn stale_pong_cannot_change_flow_or_session_deadline() {
    let (mut control, io) = established();
    control.send_ping(9, 10).await.unwrap();
    let mut stale = pong();
    stale.client_addr.set_port(9999);
    response(&io, 8, ControlResponse::Pong(stale));
    assert!(matches!(
        control.recv_feed_msg().await,
        Err(errors::ControlError::UnmatchedResponse)
    ));
    assert_eq!(control.is_expired(), None);
    assert_eq!(control.conn.pong_latest.client_addr.port(), 1234);
}

#[tokio::test(start_paused = true)]
async fn expired_server_timestamp_does_not_underflow() {
    let (mut control, io) = established();
    control.send_ping(9, 10).await.unwrap();
    let mut expired = pong();
    expired.session_expire_at = Some(999);
    response(&io, 9, ControlResponse::Pong(expired));
    control.recv_feed_msg().await.unwrap();
    assert_eq!(control.is_expired(), Some(ExpiredReason::Deadline));
}

#[tokio::test(start_paused = true)]
async fn delayed_ping_still_matches_after_next_send_attempt() {
    let (mut control, io) = established();
    control.send_ping(9, 10).await.unwrap();
    tokio::time::advance(Duration::from_millis(1500)).await;
    control.send_ping(10, 20).await.unwrap();
    let mut pong = pong();
    pong.session_expire_at = Some(61_000);
    response(&io, 9, ControlResponse::Pong(pong));
    control.recv_feed_msg().await.unwrap();
    assert_eq!(control.current_ping, Some(1500));
}

#[tokio::test]
async fn registration_preserves_interleaved_new_client_events() {
    let io = FakeIo::default();
    let client = ControlFeed::NewClient(NewClient {
        connect_addr: pong().tunnel_addr,
        peer_addr: pong().client_addr,
        data_center_id: 1,
        tunnel_id: 1,
        port_offset: 0,
        claim_instructions: ClaimInstructions {
            address: pong().tunnel_addr,
            token: vec![1],
        },
    });
    io.0.lock().unwrap().push_back(client.clone());
    // No registration reply arrives, but cancellation must preserve the queued event.
    let mut connected = ConnectedControl::new(pong().tunnel_addr, io, pong());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), connected.authenticate(&Auth))
            .await
            .is_err()
    );
    assert_eq!(connected.recv().await.unwrap(), client);
}
