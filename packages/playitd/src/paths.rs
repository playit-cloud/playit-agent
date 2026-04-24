use std::path::{Path, PathBuf};

pub fn default_secret_path() -> PathBuf {
    if Path::new("playit.toml").exists() {
        return PathBuf::from("playit.toml");
    }

    #[cfg(target_os = "linux")]
    if let Some(path) = linux_default_secret_path() {
        return path;
    }

    playit_ipc::paths::playit_config_dir().join("playit.toml")
}

#[cfg(target_os = "linux")]
pub(crate) fn linux_default_secret_path() -> Option<PathBuf> {
    let path = PathBuf::from("/etc/playit/playit.toml");
    path.exists().then_some(path)
}

#[cfg(target_os = "macos")]
pub fn macos_launch_agent_data_dir() -> PathBuf {
    playit_ipc::paths::playit_config_dir()
}

#[cfg(target_os = "macos")]
pub fn macos_launch_agent_secret_path() -> PathBuf {
    macos_launch_agent_data_dir().join("playit.toml")
}

#[cfg(target_os = "macos")]
pub fn macos_launch_agent_socket_path() -> PathBuf {
    playit_ipc::paths::macos_launch_agent_socket_path()
}

#[cfg(target_os = "macos")]
pub fn macos_launch_agent_log_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| ".".into())
        .join("Library")
        .join("Logs")
        .join("playit")
}

#[cfg(target_os = "macos")]
pub fn macos_launch_agent_log_path() -> PathBuf {
    macos_launch_agent_log_dir().join("playitd.log")
}

#[cfg(target_os = "windows")]
pub fn windows_service_data_dir() -> PathBuf {
    std::env::var_os("PROGRAMDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("playit_gg")
}

#[cfg(target_os = "windows")]
pub fn windows_service_secret_path() -> PathBuf {
    windows_service_data_dir().join("playit.toml")
}

#[cfg(target_os = "windows")]
pub fn windows_service_log_path() -> PathBuf {
    windows_service_data_dir().join("logs").join("playitd.log")
}

#[cfg(test)]
mod tests {
    use super::default_secret_path;

    #[test]
    fn fallback_secret_path_uses_playit_config_dir() {
        if std::path::Path::new("playit.toml").exists() {
            return;
        }

        #[cfg(target_os = "linux")]
        if std::path::Path::new("/etc/playit/playit.toml").exists() {
            return;
        }

        assert!(default_secret_path().ends_with("playit.toml"));
    }
}
