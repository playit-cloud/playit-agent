use std::sync::OnceLock;

use playit_api_client::api::AgentVersion;

pub static AGENT_VERSION: OnceLock<AgentVersion> = OnceLock::new();

pub fn register_version(version: AgentVersion) {
    AGENT_VERSION.get_or_init(|| version);
}

pub fn get_version() -> AgentVersion {
    AGENT_VERSION.get().expect("version not registered").clone()
}
