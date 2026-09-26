use crate::media::MediaError;
use crate::wayland::screencast::StopCause;
use crate::wayland::virtual_output::OutputCloseCause;
use crate::wayland::WaylandError;

#[derive(Debug, thiserror::Error)]
pub enum FeatureError {
    #[error(transparent)]
    Media(#[from] MediaError),
    #[error(transparent)]
    Wayland(#[from] WaylandError),
    #[error(
        "no v4l2loopback camera: load the v4l2loopback module or set CROWNCONNECT_CAMERA_DEVICE"
    )]
    NoLoopbackCamera,
    #[error("the screen capture stopped ({0:?})")]
    CaptureStopped(StopCause),
    #[error("the virtual output closed ({0:?})")]
    OutputClosed(OutputCloseCause),
    #[error("the compositor did not create the virtual output in time")]
    OutputTimedOut,
    #[error("the viewer window was closed")]
    ViewerClosed,
    #[error("{0} is not supported on this computer")]
    Unsupported(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
