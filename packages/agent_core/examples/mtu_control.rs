// Example:
// PLAYIT_API_URL="https://api.playit.gg" \
// PLAYIT_SECRET_KEY="..." \
// PLAYIT_MTU_DC_IDS="1,2" \
// cargo run -p playit-agent-core --example mtu_control
//
// The example connects to control, sends a few MTU probes, waits briefly for
// responses, then prints both pending and committed MTU discovery state.
use std::env;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use playit_agent_core::agent_control::address_selector::AddressSelector;
use playit_agent_core::agent_control::{AuthApi, AuthResource, DualStackUdpSocket};

const CHECK_MTU_SIZES: [u32; 5] = [1200, 1300, 1400, 1420, 1480];
const MTU_TEST_SIZES: [u32; 5] = [1200, 1300, 1400, 1420, 1450];
const RECV_TIMEOUT: Duration = Duration::from_secs(5);
const RECV_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let api_url = required_env("PLAYIT_API_URL")?;
    let secret_key = required_env("PLAYIT_SECRET_KEY")?;
    let data_center_ids = parse_data_center_ids()?;

    println!("creating udp io");
    let io = DualStackUdpSocket::new()
        .await
        .map_err(|error| format!("failed to create UDP socket: {error}"))?;

    println!("fetching control addresses");
    let auth = AuthApi::new(api_url, secret_key);
    let control_addresses = auth
        .get_control_addresses()
        .await
        .map_err(|error| format!("failed to fetch control addresses: {error:?}"))?;

    println!("connecting to control");
    let connected = AddressSelector::new(control_addresses, io)
        .connect_to_first()
        .await
        .map_err(|error| format!("failed to connect to control: {error:?}"))?;

    println!("authenticating established control");
    let mut control = connected
        .auth_into_established(auth)
        .await
        .map_err(|error| format!("failed to authenticate control: {error:?}"))?;

    control.clear_pending_mtu_data();

    let mut next_request_id = time_id_seed();

    println!("sending CheckMtuReceived probes");
    for message_size in CHECK_MTU_SIZES {
        let request_id = take_next_id(&mut next_request_id);
        let test_id = take_next_id(&mut next_request_id);

        control
            .send_check_mtu_received(request_id, test_id, message_size)
            .await
            .map_err(|error| {
                format!("failed to send CheckMtuReceived({message_size}): {error:?}")
            })?;
    }

    println!("sending SendMtuTest probes");
    for data_center_id in &data_center_ids {
        for udp_payload_length in MTU_TEST_SIZES {
            let request_id = take_next_id(&mut next_request_id);
            let test_id = take_next_id(&mut next_request_id);

            control
                .send_mtu_test(request_id, test_id, *data_center_id, udp_payload_length)
                .await
                .map_err(|error| {
                    format!(
                        "failed to send SendMtuTest(dc={data_center_id}, payload={udp_payload_length}): {error:?}"
                    )
                })?;
        }
    }

    println!("waiting up to {:?} for MTU responses", RECV_TIMEOUT);
    let deadline = Instant::now() + RECV_TIMEOUT;
    while Instant::now() < deadline {
        match tokio::time::timeout(RECV_POLL_INTERVAL, control.recv_feed_msg()).await {
            Ok(Ok(feed)) => println!("received feed: {feed:?}"),
            Ok(Err(error)) => eprintln!("control receive error: {error:?}"),
            Err(_) => {}
        }
    }

    println!("pending_mtu_data: {:#?}", control.pending_mtu_data());
    control.commit_pending_mtu_data();
    println!("known_mtu_data: {:#?}", control.known_mtu_data());

    Ok(())
}

fn required_env(name: &str) -> Result<String, String> {
    env::var(name).map_err(|_| format!("missing required environment variable `{name}`"))
}

fn parse_data_center_ids() -> Result<Vec<u32>, String> {
    let raw = required_env("PLAYIT_MTU_DC_IDS")?;
    let mut ids = Vec::new();

    for part in raw.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed = trimmed
            .parse::<u32>()
            .map_err(|error| format!("invalid data center id `{trimmed}`: {error}"))?;
        ids.push(parsed);
    }

    if ids.is_empty() {
        return Err(
            "`PLAYIT_MTU_DC_IDS` must contain at least one comma-separated data center id".into(),
        );
    }

    Ok(ids)
}

fn time_id_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn take_next_id(next: &mut u64) -> u64 {
    let current = *next;
    *next = (*next).saturating_add(1);
    current
}
