//! The peer runtime, which owns llts sessions with paired devices.
//!
//! Everything that needs a paired device goes through [`PeerCommand`]; everything the runtime
//! learns about devices comes back as a [`DaemonEvent`](crate::ipc::server::DaemonEvent).

mod command;
pub(crate) mod convert;
mod feature_store;
pub mod identity;
pub mod link_monitor;
pub mod media_coordinator;
pub mod runtime;
mod wakeup;

pub use command::{PeerCommand, PeerNotice, PeerRequest};
pub use media_coordinator::{
    EncodedFrame, LoggingMediaCoordinator, MediaCoordinator, MediaOutput, UnicursorSignal,
};
pub use runtime::{PeerRuntimeError, PeerServices};
pub use wakeup::RuntimeWaker;
