pub mod daemon;
mod ipc_server;
#[cfg(target_os = "linux")]
pub(crate) mod linux;
pub mod logging;
pub mod manager;
pub mod paths;
#[cfg(unix)]
pub mod unix_account;
#[cfg(target_os = "windows")]
pub mod windows;

pub use daemon::{
    DEFAULT_VARIANT_ID, DaemonOptions, VersionDetails, VersionOverrideFile, load_version_overrides,
    run_daemon,
};
pub use paths::default_secret_path;

#[cfg(target_os = "macos")]
pub use paths::{
    macos_launch_agent_data_dir, macos_launch_agent_log_dir, macos_launch_agent_log_path,
    macos_launch_agent_secret_path, macos_launch_agent_socket_path,
};

#[cfg(target_os = "windows")]
pub use paths::{windows_service_data_dir, windows_service_log_path, windows_service_secret_path};
