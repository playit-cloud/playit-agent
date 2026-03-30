pub mod daemon;
mod ipc_server;
pub mod logging;
pub mod manager;

pub use daemon::{
    DEFAULT_VARIANT_ID, DaemonOptions, VersionDetails, VersionOverrideFile, default_secret_path,
    load_version_overrides, run_daemon,
};

#[cfg(target_os = "windows")]
pub use daemon::{windows_service_data_dir, windows_service_log_path, windows_service_secret_path};
