//! Phone-screen mirroring on this desktop: decoded frames are shown in a plain xdg-toplevel by
//! attaching their dmabufs directly (no GPU copy), and the viewer's pointer, keyboard and touch
//! input is sent back to the daemon as compact postcard messages.

pub mod access_unit;
mod error;
pub mod input;
mod wayland;

pub use error::PresentError;
pub use wayland::{PresentationStats, Presenter};
