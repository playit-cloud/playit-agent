pub mod paths;
pub mod secret;
pub mod service;

#[cfg(any(target_os = "windows", test))]
mod migration;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(unix)]
pub mod unix;
#[cfg(target_os = "windows")]
pub mod windows;

pub use paths::default_secret_path;
