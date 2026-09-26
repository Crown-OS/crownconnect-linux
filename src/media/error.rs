use thiserror::Error;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("{operation} failed: {source}")]
    Ffmpeg {
        operation: &'static str,
        #[source]
        source: ffmpeg_next::Error,
    },
    #[error("VA-API {operation} failed with status {status:#x}")]
    VaApi {
        operation: &'static str,
        status: i32,
    },
    #[error("no hardware codec is supported by both this device and the peer")]
    NoCommonCodec,
    #[error("not supported: {0}")]
    Unsupported(&'static str),
    #[error("invalid frame: {0}")]
    InvalidFrame(&'static str),
    #[error("opus: {0}")]
    Opus(#[from] opus::Error),
    #[error("pipewire: {0}")]
    PipeWire(#[from] pipewire::Error),
    #[error("pipewire thread exited before the stream was ready")]
    PipeWireThreadGone,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<rustix::io::Errno> for MediaError {
    fn from(errno: rustix::io::Errno) -> Self {
        Self::Io(errno.into())
    }
}
