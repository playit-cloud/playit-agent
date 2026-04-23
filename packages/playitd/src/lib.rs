pub mod daemon;
mod ipc_server;
#[cfg(target_os = "linux")]
pub(crate) mod linux;
pub mod logging;
pub mod manager;
#[cfg(target_os = "windows")]
pub mod windows;

pub use daemon::{
    DEFAULT_VARIANT_ID, DaemonOptions, VersionDetails, VersionOverrideFile, default_secret_path,
    load_version_overrides, run_daemon,
};

#[cfg(target_os = "macos")]
pub use daemon::{
    macos_launch_agent_data_dir, macos_launch_agent_log_dir, macos_launch_agent_log_path,
    macos_launch_agent_secret_path, macos_launch_agent_socket_path,
};

#[cfg(target_os = "windows")]
pub use daemon::{windows_service_data_dir, windows_service_log_path, windows_service_secret_path};
