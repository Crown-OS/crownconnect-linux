use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::event::{eventfd, EventfdFlags};

/// An eventfd that wakes the peer thread's `poll` when another thread queued work for it.
#[derive(Debug)]
pub(crate) struct Wakeup(OwnedFd);

impl Wakeup {
    pub(crate) fn new() -> std::io::Result<Self> {
        Ok(Self(eventfd(
            0,
            EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK,
        )?))
    }

    pub(crate) fn notify(&self) {
        let _ = rustix::io::write(&self.0, &1_u64.to_ne_bytes());
    }

    pub(crate) fn clear(&self) {
        let mut count = [0_u8; 8];
        let _ = rustix::io::read(&self.0, &mut count);
    }
}

impl AsFd for Wakeup {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

/// Rings the peer thread from a media pipeline, so what it queued is sent without waiting for
/// the next socket event or timer.
#[derive(Debug, Clone)]
pub struct RuntimeWaker(std::sync::Arc<Wakeup>);

impl RuntimeWaker {
    pub(crate) const fn new(wakeup: std::sync::Arc<Wakeup>) -> Self {
        Self(wakeup)
    }

    pub fn wake(&self) {
        self.0.notify();
    }
}
