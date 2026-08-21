//! Versioned IPC protocol shared by playit daemon clients and servers.
//!
//! This boundary owns wire models and transport helpers. It must not depend on
//! the agent runtime, API client, daemon implementation, or UI crates.

pub mod endpoint;
pub mod ipc;
pub mod model;
pub mod paths;
pub const ACCOUNT_AGENTS_URL: &str = "https://playit.gg/account/agents";
pub const ACCOUNT_UPGRADE_URL: &str = "https://playit.gg/account/upgrade";
pub const AGENT_OVER_LIMIT_TITLE: &str =
    "The playit service cannot start because this account is over the agent limit.";

pub fn agent_over_limit_guidance() -> String {
    format!(
        "Delete unused agents: {ACCOUNT_AGENTS_URL}\nIncrease your agent limit: {ACCOUNT_UPGRADE_URL}"
    )
}
