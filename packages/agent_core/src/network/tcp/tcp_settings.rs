#[derive(Clone, Debug)]
pub struct TcpSettings {
    pub new_client_ratelimit: u32,
    pub new_client_ratelimit_burst: u32,
    pub tcp_no_delay: bool,
}

pub const DEFAULT_NEW_CLIENT_RATELIMIT: u32 = 5;

impl Default for TcpSettings {
    fn default() -> Self {
        Self {
            new_client_ratelimit: DEFAULT_NEW_CLIENT_RATELIMIT,
            new_client_ratelimit_burst: 32,
            tcp_no_delay: true,
        }
    }
}
