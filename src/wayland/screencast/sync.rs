//! Explicit synchronisation with the compositor over two timelines: it signals frames on the
//! acquire timeline, this client signals releases on the release timeline, and neither ever
//! signals the other's.

use std::os::fd::{AsFd, BorrowedFd};
use std::sync::Arc;
use std::time::Duration;

use crownos_protocols::screencast::v1::client::crownos_screencast_session_v1::CrownosScreencastSessionV1;

use crate::util::syncobj::{SyncobjError, Timeline};

#[derive(Debug)]
pub(super) struct ExplicitSync {
    acquire: Arc<Timeline>,
    release: Timeline,
    last_release_point: u64,
}

impl ExplicitSync {
    /// Creates both timelines on `render_node` and hands them to `session`, which must not have
    /// started yet.
    pub(super) fn attach(
        render_node: BorrowedFd<'_>,
        session: &CrownosScreencastSessionV1,
    ) -> Result<Self, SyncobjError> {
        let acquire = Timeline::create(render_node)?;
        let release = Timeline::create(render_node)?;
        session.set_timelines(acquire.export()?.as_fd(), release.export()?.as_fd());
        Ok(Self {
            acquire: Arc::new(acquire),
            release,
            last_release_point: 0,
        })
    }

    pub(super) fn acquire_point(&self, point: u64) -> AcquirePoint {
        AcquirePoint {
            timeline: Arc::clone(&self.acquire),
            point,
        }
    }

    /// Signals the next release point and returns it for the release request, so release points
    /// start above zero and strictly increase as the protocol requires.
    pub(super) fn signal_release(&mut self) -> Result<u64, SyncobjError> {
        let point = self.last_release_point + 1;
        self.release.signal(point)?;
        self.last_release_point = point;
        Ok(point)
    }
}

/// The point on the compositor's acquire timeline at which a frame is fully rendered.
#[derive(Debug, Clone)]
pub struct AcquirePoint {
    timeline: Arc<Timeline>,
    point: u64,
}

impl AcquirePoint {
    pub const fn point(&self) -> u64 {
        self.point
    }

    /// Blocks until the frame is rendered; call it on the encoder's thread, never on the async
    /// runtime.
    ///
    /// # Errors
    ///
    /// [`SyncobjError::TimedOut`] when rendering takes longer than `timeout`.
    pub fn wait(&self, timeout: Duration) -> Result<(), SyncobjError> {
        self.timeline.wait(self.point, timeout)
    }
}
