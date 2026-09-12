#[derive(Clone, Debug)]
pub struct TcpSettings {
    pub new_client_ratelimit: u32,
    pub new_client_ratelimit_burst: u32,
    pub tcp_no_delay: bool,
    pub max_pending_clients: usize,
    pub max_clients: usize,
    pub idle_timeout: std::time::Duration,
}

impl Default for TcpSettings {
    fn default() -> Self {
        Self {
            new_client_ratelimit: 100,
            new_client_ratelimit_burst: 300,
            tcp_no_delay: true,
            max_pending_clients: 128,
            max_clients: 8192,
            idle_timeout: std::time::Duration::from_secs(90),
        }
    }
}
