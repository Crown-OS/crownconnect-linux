//! Hardware-accelerated media: VA-API video, Opus voice, PipeWire nodes, a v4l2loopback camera
//! sink and a Wayland presenter for mirrored screens.

pub mod audio;
mod error;
pub mod present;
pub mod v4l2_sink;
pub mod video;

pub use error::MediaError;
