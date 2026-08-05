//! Core playit agent runtime: control-plane maintenance and TCP/UDP tunneling.
//!
//! This crate may depend on the protocol and API-client crates. It must not
//! depend on daemon, IPC presentation, or platform service-management layers.

extern crate core;

pub mod agent_control;
pub mod network;
pub mod playit_agent;
pub mod stats;
pub mod utils;

pub const PROTOCOL_VERSION: u64 = 2;
