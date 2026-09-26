//! This computer's own state, as it publishes it to paired devices.
//!
//! Each publisher watches one platform service and sends a [`LocalState`] whenever what it
//! watches changes. The peer runtime folds them into an llts `StatePublisher`, which drops
//! values that encode to what was already published.

mod controls;
mod local_state;
pub mod publishers;
pub mod session_lock;
mod sink;

pub use controls::{LocalCommandReceivers, LocalControls};
pub use local_state::{ClipboardSnapshot, HotspotSnapshot, LocalState, MediaSnapshot};
pub use sink::{state_channel, StateSink};
