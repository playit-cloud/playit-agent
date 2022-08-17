#[cfg(feature = "use-ring")]
mod use_ring;

#[cfg(feature = "use-ring")]
pub use use_ring::*;

#[cfg(all(not(feature = "use-ring"), feature = "use-hmac"))]
mod use_hmac;

#[cfg(all(not(feature = "use-ring"), feature = "use-hmac"))]
pub use use_hmac::*;