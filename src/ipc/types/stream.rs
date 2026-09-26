use serde::{Deserialize, Serialize};

/// Which way a stream flows, seen from this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    ToPeer,
    FromPeer,
}

/// A side of this computer's desktop that a peer's screen or cursor attaches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// Which camera on the peer to stream from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CameraLens {
    Back,
    Front,
}

/// Upper bounds for a video stream; the daemon settles on the best mode both ends support
/// within them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoLimits {
    pub max_width: u16,
    pub max_height: u16,
    pub max_fps: u16,
}

/// How to mirror a screen between this computer and a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MirrorOptions {
    pub direction: Direction,
    pub limits: VideoLimits,
    /// Whether the viewing side may drive the mirrored screen's pointer and keyboard.
    pub remote_input: bool,
}

/// How to use a peer as a camera for this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CameraOptions {
    pub lens: CameraLens,
    pub limits: VideoLimits,
}

/// How to use a peer as an extra monitor for this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MonitorOptions {
    pub placement: Edge,
    pub limits: VideoLimits,
}
