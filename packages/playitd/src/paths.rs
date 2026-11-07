use std::path::{Path, PathBuf};

/// Where TLS certificates for HTTPS tunnels are kept when `--cert-dir` isn't given: a
/// `certs` folder beside the secret file, which the daemon can already write to.
pub fn default_cert_dir(secret_path: Option<&Path>) -> PathBuf {
    secret_path
        .and_then(|path| path.parent())
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| match secret_path {
            Some(_) => PathBuf::from("."),
            None => playit_ipc::paths::playit_config_dir(),
        })
        .join("certs")
}

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

/// launchd writes panics and other output that bypasses tracing here.
#[cfg(target_os = "macos")]
pub fn macos_launch_agent_stderr_path() -> PathBuf {
    macos_launch_agent_log_dir().join("playitd.stderr.log")
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
pub fn windows_service_log_dir() -> PathBuf {
    windows_service_data_dir().join("logs")
}

#[cfg(target_os = "windows")]
pub fn windows_service_log_path() -> PathBuf {
    windows_service_log_dir().join("playitd.log")
}

/// Where `playit` keeps its secret when no background service is installed (portable
/// mode). This is also where playit 0.17 stored its secret.
#[cfg(target_os = "windows")]
pub fn windows_portable_secret_path() -> PathBuf {
    playit_ipc::paths::playit_config_dir().join("playit.toml")
}

#[cfg(target_os = "windows")]
pub fn windows_portable_log_path() -> PathBuf {
    playit_ipc::paths::playit_config_dir()
        .join("logs")
        .join("playitd.log")
}

#[cfg(test)]
mod tests {
    use super::default_cert_dir;
    use std::path::{Path, PathBuf};

    #[test]
    fn cert_dir_sits_beside_the_secret() {
        assert_eq!(
            default_cert_dir(Some(Path::new("/etc/playit/playit.toml"))),
            PathBuf::from("/etc/playit/certs")
        );
        assert_eq!(
            default_cert_dir(Some(Path::new("playit.toml"))),
            PathBuf::from("./certs")
        );
        assert_eq!(
            default_cert_dir(None),
            playit_ipc::paths::playit_config_dir().join("certs")
        );
    }
}
