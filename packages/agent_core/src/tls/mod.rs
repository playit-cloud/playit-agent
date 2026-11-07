//! TLS termination for HTTPS tunnels: certificates from Let's Encrypt (HTTP-01) and a
//! reverse proxy that forwards decrypted requests to the local web server.

pub mod manager;
pub mod proxy;
pub mod store;

pub use manager::{CertManager, CertStatus};
pub use store::CertStore;
