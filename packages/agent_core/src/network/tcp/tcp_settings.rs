#[derive(Clone, Debug)]
pub struct TcpSettings {
    pub new_client_ratelimit: u32,
    pub new_client_ratelimit_burst: u32,
    pub tcp_no_delay: bool,
}

impl Default for TcpSettings {
    fn default() -> Self {
        Self {
            new_client_ratelimit: 100,
            new_client_ratelimit_burst: 300,
            tcp_no_delay: true,
        }
    }
}
