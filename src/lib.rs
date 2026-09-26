//! CrownOS cross-device hub.
//!
//! Built with `default-features = false`, the library is only the
//! [`ipc::proto`] schema of the `crownconnect` service, so clients get the
//! typed contract without the daemon.

pub mod ipc;
#[cfg(feature = "media")]
pub mod media;
#[cfg(feature = "media")]
pub mod util;

#[cfg(feature = "daemon")]
pub mod bluetooth;
#[cfg(feature = "daemon")]
pub mod config;
#[cfg(feature = "daemon")]
pub mod discovery;
#[cfg(feature = "daemon")]
pub mod features;
#[cfg(feature = "daemon")]
pub mod pairing;
#[cfg(feature = "daemon")]
pub mod peer;
#[cfg(feature = "daemon")]
pub mod state;
#[cfg(feature = "daemon")]
pub mod wayland;
