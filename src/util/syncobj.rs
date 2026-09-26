//! DRM timeline syncobjs: created on a render node, exported to another process, signalled from
//! the CPU and waited on with a deadline.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use drm::control::{syncobj, Device as ControlDevice};
use rustix::io::Errno;
use rustix::time::{clock_gettime, ClockId};

const NANOS_PER_SECOND: i64 = 1_000_000_000;

#[derive(Debug, thiserror::Error)]
pub enum SyncobjError {
    #[error("cannot duplicate the render node: {0}")]
    Duplicate(#[source] io::Error),
    #[error("cannot create a timeline syncobj: {0}")]
    Create(#[source] io::Error),
    #[error("cannot export a timeline syncobj: {0}")]
    Export(#[source] io::Error),
    #[error("cannot signal timeline point {point}: {source}")]
    Signal { point: u64, source: io::Error },
    #[error("cannot wait for timeline point {point}: {source}")]
    Wait { point: u64, source: io::Error },
    #[error("timeline point {0} was not signalled in time")]
    TimedOut(u64),
}

#[derive(Debug)]
struct RenderNode(OwnedFd);

impl AsFd for RenderNode {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for RenderNode {}
impl ControlDevice for RenderNode {}

/// A timeline syncobj, owned through its own handle on a render node.
#[derive(Debug)]
pub struct Timeline {
    node: RenderNode,
    handle: syncobj::Handle,
}

impl Timeline {
    /// # Errors
    ///
    /// Fails when `render_node` is not a DRM device that supports syncobjs.
    pub fn create(render_node: BorrowedFd<'_>) -> Result<Self, SyncobjError> {
        let node = render_node
            .try_clone_to_owned()
            .map(RenderNode)
            .map_err(SyncobjError::Duplicate)?;
        let handle = node.create_syncobj(false).map_err(SyncobjError::Create)?;
        Ok(Self { node, handle })
    }

    /// A descriptor another process imports the same syncobj from.
    ///
    /// # Errors
    ///
    /// Fails when the process is out of file descriptors.
    pub fn export(&self) -> Result<OwnedFd, SyncobjError> {
        self.node
            .syncobj_to_fd(self.handle, false)
            .map_err(SyncobjError::Export)
    }

    /// Signals `point`, and with it every lower point.
    ///
    /// # Errors
    ///
    /// Fails when the kernel refuses the point.
    pub fn signal(&self, point: u64) -> Result<(), SyncobjError> {
        self.node
            .syncobj_timeline_signal(&[self.handle], &[point])
            .map_err(|source| SyncobjError::Signal { point, source })
    }

    /// Blocks until `point` is signalled, also while nothing has been submitted for it yet, or
    /// until `timeout` passes.
    ///
    /// # Errors
    ///
    /// [`SyncobjError::TimedOut`] once `timeout` passes.
    pub fn wait(&self, point: u64, timeout: Duration) -> Result<(), SyncobjError> {
        let wait_all = true;
        let wait_for_submit = true;
        let wait_available = false;
        match self.node.syncobj_timeline_wait(
            &[self.handle],
            &[point],
            monotonic_deadline(timeout),
            wait_all,
            wait_for_submit,
            wait_available,
        ) {
            Ok(_) => Ok(()),
            Err(error) if error.raw_os_error() == Some(Errno::TIME.raw_os_error()) => {
                Err(SyncobjError::TimedOut(point))
            }
            Err(source) => Err(SyncobjError::Wait { point, source }),
        }
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        let _ = self.node.destroy_syncobj(self.handle);
    }
}

/// The CLOCK_MONOTONIC nanosecond the syncobj wait ioctl takes as its absolute deadline.
fn monotonic_deadline(timeout: Duration) -> i64 {
    let now = clock_gettime(ClockId::Monotonic);
    now.tv_sec
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_add(now.tv_nsec)
        .saturating_add(i64::try_from(timeout.as_nanos()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    const RENDER_NODE: &str = "/dev/dri/renderD128";

    fn render_node() -> Option<File> {
        File::options()
            .read(true)
            .write(true)
            .open(RENDER_NODE)
            .inspect_err(|_| eprintln!("skipping: no {RENDER_NODE}"))
            .ok()
    }

    #[test]
    fn a_signalled_point_covers_every_lower_point_and_nothing_above() -> Result<(), SyncobjError> {
        let Some(node) = render_node() else {
            return Ok(());
        };
        let timeline = Timeline::create(node.as_fd())?;
        timeline.export()?;
        timeline.signal(2)?;
        timeline.wait(1, Duration::ZERO)?;
        timeline.wait(2, Duration::ZERO)?;
        assert!(matches!(
            timeline.wait(3, Duration::from_millis(1)),
            Err(SyncobjError::TimedOut(3))
        ));
        Ok(())
    }

    #[test]
    fn the_deadline_lies_ahead_of_now() {
        let now = monotonic_deadline(Duration::ZERO);
        assert!(monotonic_deadline(Duration::from_secs(1)) >= now + NANOS_PER_SECOND);
        assert!(monotonic_deadline(Duration::MAX) > now);
    }
}
