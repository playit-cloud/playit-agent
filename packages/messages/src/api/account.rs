use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
pub enum AccountApiRequest {
    #[serde(rename = "claim-agent")]
    ClaimAgent(ClaimAgent),
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct ClaimAgent {
    pub claim_key: String,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
pub enum AccountApiResponse {
    #[serde(rename = "agent-claimed")]
    AgentClaimedStatus {
        status: AgentClaimedStatus,
    },
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum AgentClaimedStatus {
    #[serde(rename = "secret-created")]
    SecretCreated,
    #[serde(rename = "secret-claimed")]
    SecretClaimed,
}



