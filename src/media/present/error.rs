use thiserror::Error;

use crate::media::MediaError;

#[derive(Debug, Error)]
pub enum PresentError {
    #[error(transparent)]
    Media(#[from] MediaError),
    #[error("cannot connect to the Wayland compositor: {0}")]
    Connect(#[from] wayland_client::ConnectError),
    #[error("Wayland registry: {0}")]
    Registry(#[from] wayland_client::globals::GlobalError),
    #[error("compositor lacks a required global: {0}")]
    MissingGlobal(#[from] wayland_client::globals::BindError),
    #[error("Wayland dispatch: {0}")]
    Dispatch(#[from] wayland_client::DispatchError),
    #[error("Wayland connection: {0}")]
    Connection(#[from] wayland_client::backend::WaylandError),
    #[error("compositor cannot import NV12 dmabufs with modifier {0:#x}")]
    UnsupportedModifier(u64),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<rustix::io::Errno> for PresentError {
    fn from(errno: rustix::io::Errno) -> Self {
        Self::Io(errno.into())
    }
}
