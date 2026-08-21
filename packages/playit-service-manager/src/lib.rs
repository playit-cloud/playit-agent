//! Platform service control and Unix account discovery for playit applications.

#[cfg(target_os = "linux")]
mod linux;
mod manager;
#[cfg(unix)]
pub mod unix_account;

pub use manager::*;
