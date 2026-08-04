pub mod daemon;
pub mod errors;
pub mod guest_login;
mod ipc_server;
#[cfg(target_os = "linux")]
pub(crate) mod linux;
pub mod logging;
pub mod manager;
pub mod paths;
mod publisher;
mod supervisor;
#[cfg(unix)]
pub mod unix_account;
pub mod version;
#[cfg(target_os = "windows")]
pub mod windows;

pub use daemon::{DaemonOptions, run_daemon};
pub use errors::DaemonError;
pub use paths::default_secret_path;
pub use version::{
    DEFAULT_VARIANT_ID, VersionDetails, VersionOverrideFile, load_version_overrides,
};

#[cfg(target_os = "macos")]
pub use paths::{
    macos_launch_agent_data_dir, macos_launch_agent_log_dir, macos_launch_agent_log_path,
    macos_launch_agent_secret_path, macos_launch_agent_socket_path,
};

#[cfg(target_os = "windows")]
pub use paths::{windows_service_data_dir, windows_service_log_path, windows_service_secret_path};
