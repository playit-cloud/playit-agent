use std::path::Path;

pub use playit_agent_core::agent_control::version::DEFAULT_VARIANT_ID;
use playit_agent_core::agent_control::version::parse_agent_version;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct VersionDetails {
    pub variant_id: String,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl VersionDetails {
    pub fn from_cargo_package() -> Result<Self, String> {
        Self::from_version_string(env!("CARGO_PKG_VERSION"), DEFAULT_VARIANT_ID)
    }

    pub fn from_version_string(version: &str, variant_id: &str) -> Result<Self, String> {
        let parsed = parse_agent_version(version, variant_id).map_err(|error| error.to_string())?;
        Ok(Self {
            variant_id: parsed.variant_id.to_string(),
            major: parsed.version_major,
            minor: parsed.version_minor,
            patch: parsed.version_patch,
        })
    }

    pub fn apply_overrides(&mut self, overrides: VersionOverrideFile) {
        if let Some(variant_id) = overrides.variant_id {
            self.variant_id = variant_id;
        }
        if let Some(major) = overrides.major {
            self.major = major;
        }
        if let Some(minor) = overrides.minor {
            self.minor = minor;
        }
        if let Some(patch) = overrides.patch {
            self.patch = patch;
        }
    }

    pub fn version_string(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct VersionOverrideFile {
    pub variant_id: Option<String>,
    pub major: Option<u32>,
    pub minor: Option<u32>,
    pub patch: Option<u32>,
}

pub async fn load_version_overrides(path: &Path) -> Result<VersionOverrideFile, String> {
    let content = tokio::fs::read_to_string(path).await.map_err(|error| {
        format!(
            "Failed to read version override file {}: {error}",
            path.display()
        )
    })?;

    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("json") => serde_json::from_str(&content)
            .map_err(|error| format!("Invalid JSON in {}: {error}", path.display())),
        Some("yaml") | Some("yml") => serde_yml::from_str(&content)
            .map_err(|error| format!("Invalid YAML in {}: {error}", path.display())),
        _ => Err(format!(
            "Unsupported version override file format for {}. Use .json, .yaml, or .yml",
            path.display()
        )),
    }
}
