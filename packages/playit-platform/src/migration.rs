use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationStatus {
    AlreadyConfigured,
    MissingLegacy,
    Copied,
}

pub(crate) fn migrate_v17_secret_from_profile(
    new_path: &Path,
    profile_dir: &Path,
) -> Result<MigrationStatus, String> {
    if new_path.exists() {
        return Ok(MigrationStatus::AlreadyConfigured);
    }
    let old_path = v17_secret_path_from_profile(profile_dir);
    if !old_path.exists() {
        return Ok(MigrationStatus::MissingLegacy);
    }
    if let Some(parent) = new_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create playit service config directory at {}: {error}",
                parent.display()
            )
        })?;
    }
    let content = fs::read(&old_path).map_err(|error| {
        format!(
            "Failed to read legacy playit config at {}: {error}",
            old_path.display()
        )
    })?;
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(new_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Ok(MigrationStatus::AlreadyConfigured);
        }
        Err(error) => {
            return Err(format!(
                "Failed to create migrated playit config at {}: {error}",
                new_path.display()
            ));
        }
    };
    file.write_all(&content).map_err(|error| {
        format!(
            "Failed to write migrated playit config to {}: {error}",
            new_path.display()
        )
    })?;
    Ok(MigrationStatus::Copied)
}

fn v17_secret_path_from_profile(profile_dir: &Path) -> PathBuf {
    profile_dir
        .join("AppData")
        .join("Local")
        .join("playit_gg")
        .join("playit.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn legacy_path_and_contents_are_preserved() {
        let temp = TempDir::new("legacy-copy");
        let profile = temp.path().join("profile");
        let old_path = v17_secret_path_from_profile(&profile);
        let new_path = temp.path().join("program-data").join("playit.toml");
        fs::create_dir_all(old_path.parent().unwrap()).unwrap();
        fs::write(&old_path, "secret_key = \"abc\"\n").unwrap();

        assert_eq!(
            migrate_v17_secret_from_profile(&new_path, &profile).unwrap(),
            MigrationStatus::Copied
        );
        assert_eq!(
            fs::read_to_string(new_path).unwrap(),
            "secret_key = \"abc\"\n"
        );
        assert!(
            old_path.ends_with(
                PathBuf::from("AppData")
                    .join("Local")
                    .join("playit_gg")
                    .join("playit.toml")
            )
        );
    }

    #[test]
    fn migration_never_overwrites_existing_configuration() {
        let temp = TempDir::new("existing-config");
        let profile = temp.path().join("profile");
        let new_path = temp.path().join("program-data").join("playit.toml");
        fs::create_dir_all(new_path.parent().unwrap()).unwrap();
        fs::write(&new_path, "new").unwrap();

        assert_eq!(
            migrate_v17_secret_from_profile(&new_path, &profile).unwrap(),
            MigrationStatus::AlreadyConfigured
        );
        assert_eq!(fs::read_to_string(new_path).unwrap(), "new");
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "playit-platform-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
