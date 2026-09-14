//! Core runtime for a playit.gg agent.
//!
//! The agent is made of three independent pieces that [`agent::PlayitAgent`] wires together:
//!
//! * [`control`]: the UDP control channel to the tunnel server. It registers the agent,
//!   keeps the session alive, and yields [`control::ControlEvent`]s (new TCP clients, UDP
//!   session details).
//! * [`tcp`]: accepts `NewClient` events, claims the connection at the tunnel server and
//!   pipes bytes between the tunnel and the local origin.
//! * [`udp`]: the UDP data channel to the tunnel server plus the table of virtual clients
//!   that relay datagrams to local origins.
//!
//! [`origin`] resolves tunnel ids to local addresses and holds the local-socket helpers
//! shared by the TCP and UDP paths.

pub mod agent;
pub mod control;
pub mod error;
pub mod origin;
pub mod platform;
pub mod stats;
pub mod tcp;
pub mod udp;
pub mod util;

pub use agent::{AgentConfig, PlayitAgent};
pub use error::SetupError;
pub use stats::AgentStats;

pub const PROTOCOL_VERSION: u64 = 2;
