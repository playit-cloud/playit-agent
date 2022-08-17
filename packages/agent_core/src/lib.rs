pub mod agent_state;
pub mod tunnel_io;
pub mod tunnel_api;
pub mod agent_updater;
pub mod setup_config;
pub mod api_client;
pub mod dependent_task;
pub mod name_lookup;
pub mod control_lookup;
pub mod tcp_client;
pub mod lan_address;
pub mod udp_client;
pub mod ping_task;

pub fn now_milli() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}