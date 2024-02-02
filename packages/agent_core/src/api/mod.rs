use serde::Serialize;
use crate::api::api::{ApiResult, PlayitApiClient, PlayitHttpClient};
use crate::api::http_client::{HttpClient, HttpClientError};

pub mod api;
pub mod http_client;
pub mod ip_resource;

pub type PlayitApi = PlayitApiClient<HttpClient>;

impl PlayitApi {
    pub fn create(api_base: String, secret: Option<String>) -> Self {
        PlayitApiClient::new(HttpClient::new(api_base, secret.map(|v| format!("Agent-Key {}", v.trim()))))
    }
}

impl api::PortType {
    pub fn matches(&self, port: api::PortType) -> bool {
        match *self {
            api::PortType::Both => true,
            other => other == port
        }
    }
}

impl api::PortRange {
	pub fn contains(&self, port: u16) -> bool {
		self.from <= port && port < self.to
	}
}

#[cfg(test)]
mod test {
    use crate::api::api::{AgentType, ReqClaimSetup};
    use crate::api::PlayitApi;

    #[tokio::test]
    async fn test() {
        let api = PlayitApi::create("http://localhost:8080".to_string(), None);

        let res = api.claim_setup(ReqClaimSetup {
            code: "ffebed".to_string(),
            agent_type: AgentType::Assignable,
            version: "playit program v1.3.5".to_string(),
        }).await.unwrap();

        println!("{}", serde_json::to_string_pretty(&res).unwrap());
    }
}
