use std::sync::OnceLock;

use playit_api_client::api::{AgentVersion, Platform};
use std::str::FromStr;
use uuid::Uuid;

use crate::agent_control;

pub static AGENT_VERSION: OnceLock<AgentVersion> = OnceLock::new();
pub static PLATFORM: OnceLock<Platform> = OnceLock::new();

pub fn get_platform() -> Platform {
    PLATFORM
        .get_or_init(|| agent_control::current_platform())
        .clone()
}

pub fn register_platform(platform: Platform) {
    PLATFORM.get_or_init(|| platform);
}

pub fn get_version() -> AgentVersion {
    help_register_version(
        env!("CARGO_PKG_VERSION"),
        "308943e8-faef-4835-a2ba-270351f72aa3",
    )
}

pub fn help_register_version(v_str: &str, variant_id: &str) -> AgentVersion {
    AGENT_VERSION
        .get_or_init(|| {
            let mut parts = v_str.split("-").next().unwrap().split(".");

            let major = u32::from_str(parts.next().unwrap()).unwrap();
            let minor = u32::from_str(parts.next().unwrap()).unwrap();
            let patch = u32::from_str(parts.next().unwrap()).unwrap();

            AgentVersion {
                variant_id: Uuid::from_str(variant_id).expect("variant id must be UUID"),
                version_major: major,
                version_minor: minor,
                version_patch: patch,
            }
        })
        .clone()
}

pub fn register_version(version: AgentVersion) {
    AGENT_VERSION.get_or_init(|| version);
}
