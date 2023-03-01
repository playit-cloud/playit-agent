use crate::api::api::PlayitApiClient;
use crate::api::http_client::HttpClient;

pub mod api;
pub mod http_client;

pub type PlayitApi = PlayitApiClient<HttpClient>;

impl PlayitApi {
    pub fn create(api_base: String, secret: Option<String>) -> Self {
        PlayitApiClient::new(HttpClient::new(api_base, secret.map(|v| format!("Agent-Key {}", v))))
    }
}

#[cfg(test)]
mod test {
    use crate::api::api::{AgentType, ReqClaimDetails, ReqClaimSetup, ReqTunnelsList};
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
