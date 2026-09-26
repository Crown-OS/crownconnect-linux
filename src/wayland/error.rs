use wayland_client::globals::{BindError, GlobalError};
use wayland_client::{ConnectError, DispatchError};

#[derive(Debug, thiserror::Error)]
pub enum WaylandError {
    #[error("cannot reach the compositor: {0}")]
    Connect(#[from] ConnectError),
    #[error("cannot list the compositor's globals: {0}")]
    Globals(#[from] GlobalError),
    #[error("the compositor lacks {0}")]
    Bind(#[from] BindError),
    #[error("wayland dispatch: {0}")]
    Dispatch(#[from] DispatchError),
    #[error("wayland connection: {0}")]
    Backend(#[from] wayland_client::backend::WaylandError),
    #[error("the compositor has no {0}")]
    Missing(&'static str),
    #[error("the compositor would reject this {0}")]
    InvalidArgument(&'static str),
    #[error("cannot allocate capture buffers: {0}")]
    Allocation(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the wayland thread exited")]
    ThreadGone,
}

impl From<rustix::io::Errno> for WaylandError {
    fn from(errno: rustix::io::Errno) -> Self {
        Self::Io(errno.into())
    }
}
