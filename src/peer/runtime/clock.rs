use std::time::{Duration, Instant as WallClock};

use llts_core::clock::Instant;
use llts_node::Now;

use crate::util::unix_time::unix_millis_now;

/// The node's two clocks: monotonic microseconds since the runtime started, and wall time.
#[derive(Debug, Clone, Copy)]
pub(super) struct RuntimeClock {
    origin: WallClock,
}

impl RuntimeClock {
    pub(super) fn start() -> Self {
        Self {
            origin: WallClock::now(),
        }
    }

    pub(super) fn now(self) -> Now {
        let micros = u64::try_from(self.origin.elapsed().as_micros()).unwrap_or(u64::MAX);
        Now::new(Instant::from_micros(micros), unix_millis_now())
    }

    /// How long until `deadline`, zero once it passed.
    pub(super) fn until(self, deadline: Instant) -> Duration {
        deadline.saturating_elapsed_since(self.now().instant)
    }
}
