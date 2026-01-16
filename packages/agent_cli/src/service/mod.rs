//! Background service management for playit agent.
//!
//! This module provides:
//! - IPC protocol for communication between CLI and background service
//! - Daemon entry point for running the agent as a background service
//! - Service manager integration for install/uninstall/start/stop

pub mod daemon;
pub mod ipc;
pub mod manager;

pub use daemon::run_daemon;
pub use ipc::{IpcClient, IpcServer, ServiceEvent, ServiceRequest};
pub use manager::ServiceController;
