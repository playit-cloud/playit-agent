use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
use crate::sid::normalize_sid;

const PROFILE_LIST_REGISTRY_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";
#[cfg(target_os = "windows")]
const PROFILE_IMAGE_PATH_VALUE: &str = "ProfileImagePath";

#[cfg(target_os = "windows")]
pub(crate) fn migrate_v17_secret(installed_user_sid: Option<&str>) -> Result<(), String> {
    let installed_user_sid = installed_user_sid
        .map(str::trim)
        .filter(|sid| !sid.is_empty())
        .ok_or_else(|| "MSI did not provide the installing user's SID".to_string())?;
    let installed_user_sid = normalize_sid(installed_user_sid).ok_or_else(|| {
        format!("MSI provided an invalid installing user SID: {installed_user_sid}")
    })?;

    let new_path = playitd::windows_service_secret_path();
    if new_path.exists() {
        return Ok(());
    }

    let Some(profile_dir) = profile_dir_for_sid(installed_user_sid)? else {
        return Ok(());
    };

    migrate_v17_secret_from_profile(&new_path, &profile_dir).map(|_| ())
}

#[derive(Debug, PartialEq, Eq)]
enum MigrationStatus {
    AlreadyConfigured,
    MissingLegacy,
    Copied,
}

fn migrate_v17_secret_from_profile(
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

    if new_path.exists() {
        return Ok(MigrationStatus::AlreadyConfigured);
    }

    let content = fs::read(&old_path).map_err(|error| {
        format!(
            "Failed to read legacy playit config at {}: {error}",
            old_path.display()
        )
    })?;

    let mut new_file = match fs::OpenOptions::new()
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

    new_file.write_all(&content).map_err(|error| {
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

fn profile_registry_subkey(sid: &str) -> String {
    format!("{PROFILE_LIST_REGISTRY_KEY}\\{sid}")
}

#[cfg(test)]
fn expand_percent_variables_with<F>(value: &str, lookup: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let mut expanded = String::with_capacity(value.len());
    let mut remaining = value;

    while let Some(start) = remaining.find('%') {
        expanded.push_str(&remaining[..start]);
        let after_start = &remaining[start + 1..];

        let Some(end) = after_start.find('%') else {
            expanded.push('%');
            expanded.push_str(after_start);
            return expanded;
        };

        let key = &after_start[..end];
        if key.is_empty() {
            expanded.push_str("%%");
        } else if let Some(replacement) = lookup(key) {
            expanded.push_str(&replacement);
        } else {
            expanded.push('%');
            expanded.push_str(key);
            expanded.push('%');
        }

        remaining = &after_start[end + 1..];
    }

    expanded.push_str(remaining);
    expanded
}

#[cfg(target_os = "windows")]
fn profile_dir_for_sid(sid: &str) -> Result<Option<PathBuf>, String> {
    let Some(profile_image_path) = read_profile_image_path(sid)? else {
        return Ok(None);
    };

    let expanded = expand_environment_strings(&profile_image_path)?;
    Ok(Some(PathBuf::from(expanded)))
}

#[cfg(target_os = "windows")]
fn read_profile_image_path(sid: &str) -> Result<Option<String>, String> {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, REG_VALUE_TYPE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
    };
    use windows::core::PCWSTR;

    let subkey = wide_null(&profile_registry_subkey(sid));
    let value = wide_null(PROFILE_IMAGE_PATH_VALUE);
    let flags = RRF_RT_REG_EXPAND_SZ | RRF_RT_REG_SZ;
    let mut value_type = REG_VALUE_TYPE::default();
    let mut byte_len = 0u32;

    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            flags,
            Some(&mut value_type),
            None,
            Some(&mut byte_len),
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
        return Err(format!(
            "Failed to read {PROFILE_IMAGE_PATH_VALUE} size from HKLM\\{}: {}",
            profile_registry_subkey(sid),
            status.0
        ));
    }

    let mut buffer = vec![0u16; byte_len.div_ceil(2) as usize];
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            flags,
            Some(&mut value_type),
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut byte_len),
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(format!(
            "Failed to read {PROFILE_IMAGE_PATH_VALUE} from HKLM\\{}: {}",
            profile_registry_subkey(sid),
            status.0
        ));
    }

    buffer.truncate(byte_len.div_ceil(2) as usize);
    while buffer.last() == Some(&0) {
        buffer.pop();
    }

    String::from_utf16(&buffer).map(Some).map_err(|error| {
        format!(
            "Failed to decode {PROFILE_IMAGE_PATH_VALUE} from HKLM\\{}: {error}",
            profile_registry_subkey(sid)
        )
    })
}

#[cfg(target_os = "windows")]
fn expand_environment_strings(value: &str) -> Result<String, String> {
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows::core::PCWSTR;

    let source = wide_null(value);
    let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), None) };
    if needed == 0 {
        return Err(format!(
            "Failed to expand environment variables in profile path {value}: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut buffer = vec![0u16; needed as usize];
    let written = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), Some(&mut buffer)) };
    if written == 0 {
        return Err(format!(
            "Failed to expand environment variables in profile path {value}: {}",
            std::io::Error::last_os_error()
        ));
    }
    if written as usize > buffer.len() {
        return Err(format!(
            "Expanded profile path did not fit in allocated buffer for {value}"
        ));
    }

    buffer.truncate(written.saturating_sub(1) as usize);
    String::from_utf16(&buffer)
        .map_err(|error| format!("Failed to decode expanded profile path for {value}: {error}"))
}

#[cfg(target_os = "windows")]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        MigrationStatus, expand_percent_variables_with, migrate_v17_secret_from_profile,
        profile_registry_subkey, v17_secret_path_from_profile,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn profile_registry_subkey_uses_sid() {
        assert_eq!(
            profile_registry_subkey("S-1-5-21-1-2-3-1001"),
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\S-1-5-21-1-2-3-1001"
        );
    }

    #[test]
    fn profile_path_expansion_handles_system_drive() {
        let expanded = expand_percent_variables_with(r"%SystemDrive%\Users\Alice", |key| {
            (key == "SystemDrive").then(|| "C:".to_string())
        });

        assert_eq!(expanded, r"C:\Users\Alice");
    }

    #[test]
    fn old_secret_path_uses_local_app_data() {
        assert_eq!(
            v17_secret_path_from_profile(Path::new(r"C:\Users\Alice")),
            PathBuf::from(r"C:\Users\Alice")
                .join("AppData")
                .join("Local")
                .join("playit_gg")
                .join("playit.toml")
        );
    }

    #[test]
    fn migration_noops_when_new_secret_exists() {
        let temp = TempDir::new("new-exists");
        let new_path = temp.path().join("program-data").join("playit.toml");
        let profile_dir = temp.path().join("profile");
        fs::create_dir_all(new_path.parent().unwrap()).unwrap();
        fs::write(&new_path, "new").unwrap();

        let status = migrate_v17_secret_from_profile(&new_path, &profile_dir).unwrap();

        assert_eq!(status, MigrationStatus::AlreadyConfigured);
        assert_eq!(fs::read_to_string(&new_path).unwrap(), "new");
    }

    #[test]
    fn migration_noops_when_old_secret_is_missing() {
        let temp = TempDir::new("old-missing");
        let new_path = temp.path().join("program-data").join("playit.toml");
        let profile_dir = temp.path().join("profile");

        let status = migrate_v17_secret_from_profile(&new_path, &profile_dir).unwrap();

        assert_eq!(status, MigrationStatus::MissingLegacy);
        assert!(!new_path.exists());
    }

    #[test]
    fn migration_copies_old_secret_contents() {
        let temp = TempDir::new("copies-old");
        let new_path = temp.path().join("program-data").join("playit.toml");
        let profile_dir = temp.path().join("profile");
        let old_path = v17_secret_path_from_profile(&profile_dir);
        fs::create_dir_all(old_path.parent().unwrap()).unwrap();
        fs::write(&old_path, "secret_key = \"abc\"\n").unwrap();

        let status = migrate_v17_secret_from_profile(&new_path, &profile_dir).unwrap();

        assert_eq!(status, MigrationStatus::Copied);
        assert_eq!(
            fs::read_to_string(&new_path).unwrap(),
            "secret_key = \"abc\"\n"
        );
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "playitd-windows-setup-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
