use crate::api::PlayitApiClient;
use crate::http_client::HttpClient;

// mod api is auto generated
pub mod api;
pub mod http_client;
pub mod ip_resource;

pub type PlayitApi = PlayitApiClient<HttpClient>;

impl PlayitApi {
    pub fn create(api_base: String, secret: Option<String>) -> Self {
        PlayitApiClient::new(HttpClient::new(
            api_base,
            secret.map(|v| format!("Agent-Key {}", v.trim())),
        ))
    }
}

impl api::PortType {
    pub fn matches(&self, port: api::PortType) -> bool {
        match *self {
            api::PortType::Both => true,
            other => other == port,
        }
    }
}

impl api::PortRange {
    pub fn contains(&self, port: u16) -> bool {
        self.from <= port && port < self.to
    }
}
