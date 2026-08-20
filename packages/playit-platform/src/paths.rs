use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::LazyLock;

pub const LINUX_SERVICE_NAME: &str = "playit";
pub const WINDOWS_SERVICE_NAME: &str = "playitd";
pub const NATIVE_SERVICE_LABEL: &str = "gg.playit.playitd";
pub const LINUX_INSTALLED_SECRET_PATH: &str = "/etc/playit/playit.toml";
pub const LINUX_INSTALLED_SOCKET_PATH: &str = "/run/playit/playitd.sock";
pub const LINUX_INSTALLED_LOG_PATH: &str = "/var/log/playit/playit.log";
pub const WINDOWS_DATA_DIR_NAME: &str = "playit_gg";
pub const WINDOWS_TRAY_SHORTCUT_NAME: &str = "Playit Tray.lnk";

pub fn default_secret_path() -> PathBuf {
    if Path::new("playit.toml").exists() {
        return PathBuf::from("playit.toml");
    }

    #[cfg(target_os = "linux")]
    if let Some(path) = linux_default_secret_path() {
        return path;
    }

    playit_config_dir().join("playit.toml")
}

pub fn playit_config_dir() -> PathBuf {
    dirs::config_local_dir()
        .unwrap_or_else(|| ".".into())
        .join(WINDOWS_DATA_DIR_NAME)
}

pub fn default_socket_path_string() -> String {
    #[cfg(target_os = "linux")]
    {
        LINUX_INSTALLED_SOCKET_PATH.to_owned()
    }
    #[cfg(target_os = "macos")]
    {
        macos_launch_agent_socket_path().display().to_string()
    }
    #[cfg(target_os = "windows")]
    {
        r"\\.\pipe\playitd-system".to_owned()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "./playitd.sock".to_owned()
    }
}

pub fn default_socket_path() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        LINUX_INSTALLED_SOCKET_PATH
    }
    #[cfg(target_os = "macos")]
    {
        static PATH: LazyLock<String> = LazyLock::new(default_socket_path_string);
        PATH.as_str()
    }
    #[cfg(target_os = "windows")]
    {
        r"\\.\pipe\playitd-system"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "./playitd.sock"
    }
}

#[cfg(target_os = "linux")]
pub fn linux_default_secret_path() -> Option<PathBuf> {
    let path = PathBuf::from(LINUX_INSTALLED_SECRET_PATH);
    path.exists().then_some(path)
}

#[cfg(target_os = "macos")]
pub fn macos_launch_agent_data_dir() -> PathBuf {
    playit_config_dir()
}

#[cfg(target_os = "macos")]
pub fn macos_launch_agent_secret_path() -> PathBuf {
    macos_launch_agent_data_dir().join("playit.toml")
}

#[cfg(target_os = "macos")]
pub fn macos_launch_agent_socket_path() -> PathBuf {
    macos_launch_agent_data_dir().join("playitd.sock")
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
        .join(WINDOWS_DATA_DIR_NAME)
}

#[cfg(target_os = "windows")]
pub fn windows_service_secret_path() -> PathBuf {
    windows_service_data_dir().join("playit.toml")
}

#[cfg(target_os = "windows")]
pub fn windows_service_log_path() -> PathBuf {
    windows_service_data_dir().join("logs").join("playitd.log")
}

#[cfg(target_os = "windows")]
pub fn windows_installer_log_path() -> PathBuf {
    windows_service_data_dir()
        .join("logs")
        .join("playit-installer.log")
}

#[cfg(target_os = "windows")]
pub fn windows_installed_user_sid_path() -> PathBuf {
    windows_service_data_dir().join("installed_user.sid")
}
