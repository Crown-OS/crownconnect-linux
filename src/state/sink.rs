use tokio::sync::mpsc;

use crate::state::LocalState;

/// Latest-wins changes pile up only while the runtime is busy, so a short queue is enough.
const STATE_QUEUE_DEPTH: usize = 32;

/// Where publishers send local state changes.
#[derive(Debug, Clone)]
pub struct StateSink(mpsc::Sender<LocalState>);

pub fn state_channel() -> (StateSink, mpsc::Receiver<LocalState>) {
    let (sender, receiver) = mpsc::channel(STATE_QUEUE_DEPTH);
    (StateSink(sender), receiver)
}

impl StateSink {
    /// Returns `false` once the runtime is gone, which is a publisher's cue to stop.
    pub async fn publish(&self, state: LocalState) -> bool {
        self.0.send(state).await.is_ok()
    }

    /// For publishers on threads outside the runtime. A full queue drops the change, since
    /// the next one supersedes it anyway.
    pub fn publish_from_thread(&self, state: LocalState) -> bool {
        match self.0.try_send(state) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => true,
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}
