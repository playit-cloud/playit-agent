use std::sync::OnceLock;

use crate::api::api::{AgentVersion, PlayitAgentVersion};

use super::platform::get_platform;

pub static AGENT_VERSION: OnceLock<PlayitAgentVersion> = OnceLock::new();

pub fn register_version(version: PlayitAgentVersion) {
    AGENT_VERSION.get_or_init(|| version);
}

pub fn get_version() -> PlayitAgentVersion {
    AGENT_VERSION.get_or_init(|| {
        PlayitAgentVersion {
            version: AgentVersion {
                platform: get_platform(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            official: true,
            details_website: None,
        }
    }).clone()
}