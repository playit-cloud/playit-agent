use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::sync::LazyLock;

pub fn playit_config_dir() -> PathBuf {
    dirs::config_local_dir()
        .unwrap_or_else(|| ".".into())
        .join("playit_gg")
}

pub fn default_socket_path_string() -> String {
    #[cfg(target_os = "linux")]
    {
        "/run/playit/playitd.sock".to_string()
    }

    #[cfg(target_os = "macos")]
    {
        macos_launch_agent_socket_path().display().to_string()
    }

    #[cfg(target_os = "windows")]
    {
        r"\\.\pipe\playitd-system".to_string()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "./playitd.sock".to_string()
    }
}

pub(crate) fn default_socket_path_static() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "/run/playit/playitd.sock"
    }

    #[cfg(target_os = "macos")]
    {
        static MACOS_DEFAULT_SOCKET_PATH: LazyLock<String> =
            LazyLock::new(default_socket_path_string);
        MACOS_DEFAULT_SOCKET_PATH.as_str()
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

#[cfg(target_os = "macos")]
pub fn macos_launch_agent_socket_path() -> PathBuf {
    playit_config_dir().join("playitd.sock")
}
