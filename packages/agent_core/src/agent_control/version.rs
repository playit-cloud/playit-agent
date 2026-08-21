use std::fmt::{Display, Formatter};
use std::str::FromStr;

use playit_api_client::api::AgentVersion;
use uuid::Uuid;

pub const DEFAULT_VARIANT_ID: &str = "308943e8-faef-4835-a2ba-270351f72aa3";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentVersionParseError {
    MissingPart(&'static str),
    InvalidPart(&'static str, String),
    InvalidVariantId(String),
}

impl Display for AgentVersionParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingPart(part) => write!(f, "missing {part} version"),
            Self::InvalidPart(part, value) => {
                write!(f, "invalid {part} version component `{value}`")
            }
            Self::InvalidVariantId(value) => write!(f, "invalid variant UUID `{value}`"),
        }
    }
}

impl std::error::Error for AgentVersionParseError {}

pub fn parse_agent_version(
    version: &str,
    variant_id: &str,
) -> Result<AgentVersion, AgentVersionParseError> {
    let mut parts = version.split('-').next().unwrap_or(version).split('.');
    let major = parse_part(parts.next(), "major")?;
    let minor = parse_part(parts.next(), "minor")?;
    let patch = parse_part(parts.next(), "patch")?;
    let variant_id = Uuid::from_str(variant_id)
        .map_err(|_| AgentVersionParseError::InvalidVariantId(variant_id.to_string()))?;

    Ok(AgentVersion {
        variant_id,
        version_major: major,
        version_minor: minor,
        version_patch: patch,
    })
}

pub fn current_agent_version() -> AgentVersion {
    parse_agent_version(env!("CARGO_PKG_VERSION"), DEFAULT_VARIANT_ID)
        .expect("package version and built-in variant UUID must be valid")
}

fn parse_part(value: Option<&str>, name: &'static str) -> Result<u32, AgentVersionParseError> {
    let value = value.ok_or(AgentVersionParseError::MissingPart(name))?;
    value
        .parse()
        .map_err(|_| AgentVersionParseError::InvalidPart(name, value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{AgentVersionParseError, DEFAULT_VARIANT_ID, parse_agent_version};

    #[test]
    fn parses_semver_prefix_and_variant() {
        let version = parse_agent_version("1.2.3-beta.1", DEFAULT_VARIANT_ID).unwrap();
        assert_eq!(version.version_major, 1);
        assert_eq!(version.version_minor, 2);
        assert_eq!(version.version_patch, 3);
        assert_eq!(version.variant_id.to_string(), DEFAULT_VARIANT_ID);
    }

    #[test]
    fn rejects_missing_or_invalid_values() {
        assert!(matches!(
            parse_agent_version("1.2", DEFAULT_VARIANT_ID),
            Err(AgentVersionParseError::MissingPart("patch"))
        ));
        assert!(matches!(
            parse_agent_version("1.x.3", DEFAULT_VARIANT_ID),
            Err(AgentVersionParseError::InvalidPart("minor", _))
        ));
        assert!(matches!(
            parse_agent_version("1.2.3", "invalid"),
            Err(AgentVersionParseError::InvalidVariantId(_))
        ));
    }
}
