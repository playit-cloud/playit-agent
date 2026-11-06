use std::sync::OnceLock;

use playit_api_client::api::{AgentVersion, ClientInfo, Platform, ReleaseVersion};
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

/// What this agent reports when pairing and registering. Apps that bundle the agent set
/// `PLAYIT_HOST_APP_ID` to their registered app id, so users see the app's name when pairing.
pub fn client_info() -> ClientInfo {
    let version = get_version();

    ClientInfo {
        app_id: version.variant_id,
        version: ReleaseVersion {
            major: version.version_major,
            minor: version.version_minor,
            patch: version.version_patch,
        },
        platform: serde_json::to_value(get_platform())
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string()),
        proto_version: crate::PROTOCOL_VERSION as u32,
        host_app_id: std::env::var("PLAYIT_HOST_APP_ID")
            .ok()
            .and_then(|id| Uuid::from_str(id.trim()).ok()),
    }
}
