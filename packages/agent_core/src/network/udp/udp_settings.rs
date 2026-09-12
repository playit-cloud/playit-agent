#[derive(Clone, Debug)]
pub struct UdpSettings {
    pub max_clients: usize,
    pub idle_timeout: std::time::Duration,
    pub new_client_ratelimit: u32,
    pub new_client_ratelimit_burst: u32,
}

impl Default for UdpSettings {
    fn default() -> Self {
        UdpSettings {
            max_clients: 8192,
            idle_timeout: std::time::Duration::from_secs(90),
            new_client_ratelimit: 16,
            new_client_ratelimit_burst: 32,
        }
    }
}
