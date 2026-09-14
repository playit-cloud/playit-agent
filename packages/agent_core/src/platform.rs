use playit_api_client::api::{AgentVersion, Platform};
use uuid::Uuid;

/// Variant id reported for the stock agent build.
pub const DEFAULT_VARIANT_ID: Uuid = uuid::uuid!("308943e8-faef-4835-a2ba-270351f72aa3");

pub fn current_platform() -> Platform {
    #[cfg(target_os = "windows")]
    return Platform::Windows;

    #[cfg(target_os = "linux")]
    return Platform::Linux;

    #[cfg(target_os = "freebsd")]
    return Platform::Freebsd;

    #[cfg(target_os = "macos")]
    return Platform::Macos;

    #[cfg(target_os = "android")]
    return Platform::Android;

    #[cfg(target_os = "ios")]
    return Platform::Ios;

    #[allow(unreachable_code)]
    Platform::Unknown
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidVersion(pub String);

impl std::fmt::Display for InvalidVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid agent version {:?}, expected MAJOR.MINOR.PATCH",
            self.0
        )
    }
}

impl std::error::Error for InvalidVersion {}

/// Parses a `MAJOR.MINOR.PATCH` string (a `-pre` suffix is ignored).
pub fn parse_agent_version(
    version: &str,
    variant_id: Uuid,
) -> Result<AgentVersion, InvalidVersion> {
    let base = version.split('-').next().unwrap_or(version);
    let mut parts = base.split('.').map(|part| part.parse::<u32>());

    let (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(InvalidVersion(version.to_owned()));
    };

    Ok(AgentVersion {
        variant_id,
        version_major: major,
        version_minor: minor,
        version_patch: patch,
    })
}

/// Version of this crate with the default variant id.
pub fn crate_agent_version() -> AgentVersion {
    parse_agent_version(env!("CARGO_PKG_VERSION"), DEFAULT_VARIANT_ID)
        .expect("crate version is a MAJOR.MINOR.PATCH triplet")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_pre_release_versions() {
        let version = parse_agent_version("1.2.3", DEFAULT_VARIANT_ID).unwrap();
        assert_eq!(
            (
                version.version_major,
                version.version_minor,
                version.version_patch
            ),
            (1, 2, 3)
        );

        let version = parse_agent_version("4.5.6-beta.1", DEFAULT_VARIANT_ID).unwrap();
        assert_eq!(
            (
                version.version_major,
                version.version_minor,
                version.version_patch
            ),
            (4, 5, 6)
        );
    }

    #[test]
    fn rejects_malformed_versions() {
        assert!(parse_agent_version("1.2", DEFAULT_VARIANT_ID).is_err());
        assert!(parse_agent_version("1.2.3.4", DEFAULT_VARIANT_ID).is_err());
        assert!(parse_agent_version("a.b.c", DEFAULT_VARIANT_ID).is_err());
    }
}
