pub mod tcp_client;
pub mod tunnel_client;
pub mod dependent_task;
pub mod api_client;
pub mod config;
pub mod udp_client;

pub fn now_milli() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}