use std::net::IpAddr;
use chrono::{DateTime, Local, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::agent_config::PortMapping;
use crate::ClaimProto;

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum AccountApiRequest {
    #[serde(rename = "claim-agent")]
    ClaimAgent(ClaimAgent),

    #[serde(rename = "list-agents")]
    ListAgents,

    #[serde(rename = "list-port-mappings")]
    ListPortMappings,

    #[serde(rename = "list-port-leases")]
    ListPortLeases,

    #[serde(rename = "request-lease")]
    RequestLease,

    #[serde(rename = "add-port-mapping")]
    AddPortMapping(AddPortMapping),

    #[serde(rename = "set-port-mapping")]
    SetPortMapping(PortMapping),

    #[serde(rename = "remove-port-mapping")]
    RemovePortMapping(RemovePortMapping)
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct RemovePortMapping {
    pub id: Uuid,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct ClaimAgent {
    pub claim_key: String,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct AddPortMapping {
    pub mapping: PortMapping,
    pub remap_to_lease: bool,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
pub enum AccountApiResponse {
    #[serde(rename = "agent-claimed")]
    AgentClaimedStatus {
        status: AgentClaimedStatus,
    },

    #[serde(rename = "agents")]
    Agents {
        agents: Vec<Agent>,
    },

    #[serde(rename = "port-mappings")]
    PortMappings {
        mappings: Vec<PortMapping>,
    },

    #[serde(rename = "port-mapping-set")]
    PortMappingSet {
        mapping: PortMapping,
    },

    #[serde(rename = "port-leases")]
    PortLeases {
        leases: Vec<PortLease>,
    },

    #[serde(rename = "port-mapping-removed")]
    PortMappingRemoved,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub enum AgentClaimedStatus {
    #[serde(rename = "secret-created")]
    SecretCreated,
    #[serde(rename = "secret-claimed")]
    SecretClaimed,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct Agent {
    pub id: Uuid,
    pub name: String,
    pub secret_key: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct PortLease {
    pub id: Uuid,
    pub ip_address: IpAddr,
    pub stripe_sub_id: Option<String>,
    pub is_random_allocation: bool,
    pub ports: Option<Ports>,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct Ports {
    pub proto: ClaimProto,
    pub from_port: u16,
    pub to_port: u16,
}
