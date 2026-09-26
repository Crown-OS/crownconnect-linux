//! The cross-device features end to end on this computer: screen mirroring with remote input,
//! a peer's camera and microphone as local devices, a peer as an extra monitor, one cursor
//! across computers, calls and files. Every media pipeline runs on its own thread and talks to
//! the peer runtime only through bounded queues and its wakeup.

pub(crate) mod calls;
mod camera;
mod codec;
mod coordinator;
mod error;
pub mod files;
mod input_route;
mod input_wire;
mod mic;
mod mirror;
mod monitor;
mod pipeline;
pub mod platform;
mod rate;
mod screen;
pub mod sink;
pub mod source;
mod unicursor;
mod viewer;

pub use coordinator::FeatureCoordinator;
pub use error::FeatureError;
pub use platform::{InputForwarder, LinuxPlatform, MediaPlatform, MirrorWindow};
pub use screen::CaptureTarget;
pub use sink::{ReceivedFrame, VideoSink, VoiceSink, VoiceSource};
pub use source::{FrameSource, TestPatternSource};
