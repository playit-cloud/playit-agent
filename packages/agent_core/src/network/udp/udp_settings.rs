#[derive(Clone, Debug)]
pub struct UdpSettings {
    pub new_client_ratelimit: u32,
    pub new_client_ratelimit_burst: u32,
}

impl Default for UdpSettings {
    fn default() -> Self {
        UdpSettings {
            new_client_ratelimit: 16,
            new_client_ratelimit_burst: 32,
        }
    }
}
