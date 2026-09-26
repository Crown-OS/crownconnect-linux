use serde::{Deserialize, Serialize};

/// Whether a peer's player is making sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

/// What a peer is playing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The app playing it, as the peer names it.
    pub player: String,
    pub status: PlaybackStatus,
    pub position_ms: u64,
    pub duration_ms: u64,
}
