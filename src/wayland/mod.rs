//! Clients for the compositor protocols the daemon drives, each on its own thread and queue.

mod clipboard_setter;
mod error;
mod event_outlet;
pub mod injector;
pub mod input_capture;
mod input_types;
mod outputs;
pub mod screencast;
pub mod virtual_output;
mod worker;

pub use clipboard_setter::ClipboardSetter;
pub use error::WaylandError;
pub use event_outlet::{channel_outlet, EventOutlet};
pub use input_types::ScrollAxis;
