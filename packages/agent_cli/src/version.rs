use playit_api_client::api::AgentVersion;
use uuid::Uuid;

pub const CURRENT_AGENT_VERSION: AgentVersion = AgentVersion {
    /* if you want to release your own variant email support@playit.gg */
    variant_id: Uuid::from_u128(0x308943e8faef4835a2ba270351f72aa3u128),
    version_major: 0,
    version_minor: 17,
    version_patch: 0,
};
