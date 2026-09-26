//! What was copied on this computer, from the crownos-clipboard service, and making what was
//! copied on a peer this computer's selection.

use std::io::{Read, Seek, SeekFrom};
use std::os::fd::OwnedFd;
use std::sync::Arc;

use crownos_clipboard::{protocol, Changed, EntrySummary};
use crownos_ipc::adapter::tokio::AsyncClient;
use llts_signaling::state::{HybridClock, MAX_INLINE_CLIPBOARD_BYTES};
use rustix::fs::{memfd_create, MemfdFlags};
use tokio::sync::mpsc;

use crate::state::{ClipboardSnapshot, LocalState, StateSink};
use crate::util::unix_time::unix_millis_now;
use crate::wayland::ClipboardSetter;

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("crownos-clipboard: {0}")]
    Ipc(#[from] crownos_ipc::Error),
    #[error("clipboard transfer: {0}")]
    Io(#[from] std::io::Error),
}

impl From<rustix::io::Errno> for ClipboardError {
    fn from(errno: rustix::io::Errno) -> Self {
        Self::Io(errno.into())
    }
}

/// Publishes each new clipboard entry and applies clipboards arriving from peers.
///
/// Applying needs a compositor with ext-data-control; without one, peers' clipboards are
/// dropped and only this computer's copies are published.
///
/// # Errors
///
/// Fails when the crownos-clipboard service is not running.
pub async fn run(
    sink: StateSink,
    mut remote: mpsc::Receiver<ClipboardSnapshot>,
) -> Result<(), ClipboardError> {
    let mut client = AsyncClient::connect(crownos_clipboard::SERVICE)?;
    client.subscribe::<Changed>().await?;
    let setter = match ClipboardSetter::connect().await {
        Ok(setter) => Some(setter),
        Err(error) => {
            tracing::warn!(%error, "peers' clipboards cannot be applied");
            None
        }
    };
    let mut tracker = ClipboardTracker::default();
    loop {
        if let Some(snapshot) = tracker.newest(&mut client).await?
            && !sink.publish(LocalState::Clipboard(snapshot)).await
        {
            return Ok(());
        }
        tokio::select! {
            changed = client.next_event::<Changed>() => {
                changed?;
            }
            snapshot = remote.recv() => match snapshot {
                Some(snapshot) => tracker.apply_remote(setter.as_ref(), snapshot),
                None => return Ok(()),
            },
        }
    }
}

#[derive(Debug, Default)]
struct ClipboardTracker {
    clock: HybridClock,
    last_entry: Option<u64>,
    /// What a peer last put on the clipboard, so its echo from crownos-clipboard is not
    /// published back as a new local copy.
    applied_from_peer: Option<Arc<[u8]>>,
}

impl ClipboardTracker {
    async fn newest(
        &mut self,
        client: &mut AsyncClient,
    ) -> Result<Option<ClipboardSnapshot>, ClipboardError> {
        let entries = client
            .call::<protocol::list>(&protocol::list {}, Vec::new())
            .await?;
        let Some(entry) = self.unseen(entries.first()) else {
            return Ok(None);
        };
        let (mime, bytes) = fetch(client, entry.id).await?;
        if self.is_echo(&bytes) {
            return Ok(None);
        }
        Ok(Some(ClipboardSnapshot {
            mime,
            bytes,
            stamp: self.clock.tick(unix_millis_now()),
        }))
    }

    fn unseen<'e>(&mut self, newest: Option<&'e EntrySummary>) -> Option<&'e EntrySummary> {
        let entry = newest.filter(|entry| self.last_entry != Some(entry.id))?;
        self.last_entry = Some(entry.id);
        if usize::try_from(entry.bytes).map_or(true, |len| len > MAX_INLINE_CLIPBOARD_BYTES) {
            tracing::debug!(bytes = entry.bytes, "clipboard too large to sync inline");
            return None;
        }
        Some(entry)
    }

    fn is_echo(&mut self, bytes: &[u8]) -> bool {
        self.applied_from_peer
            .take()
            .is_some_and(|applied| *applied == *bytes)
    }

    fn apply_remote(&mut self, setter: Option<&ClipboardSetter>, snapshot: ClipboardSnapshot) {
        self.clock.observe(snapshot.stamp, unix_millis_now());
        let Some(setter) = setter else {
            return;
        };
        let bytes: Arc<[u8]> = snapshot.bytes.into();
        match setter.set(&snapshot.mime, Arc::clone(&bytes)) {
            Ok(()) => self.applied_from_peer = Some(bytes),
            Err(error) => tracing::warn!(%error, "cannot apply a peer's clipboard"),
        }
    }
}

async fn fetch(client: &mut AsyncClient, id: u64) -> Result<(String, Vec<u8>), ClipboardError> {
    let sink: OwnedFd = memfd_create("crownconnect-clipboard", MemfdFlags::CLOEXEC)?;
    let for_service = sink.try_clone()?;
    let mime = client
        .call::<protocol::get>(&protocol::get { id }, vec![for_service])
        .await?;
    let mut file = std::fs::File::from(sink);
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(MAX_INLINE_CLIPBOARD_BYTES).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    Ok((mime, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, bytes: u64) -> EntrySummary {
        EntrySummary {
            id,
            mimes: vec!["text/plain".into()],
            bytes,
            preview: String::new(),
        }
    }

    #[test]
    fn each_entry_is_published_once() {
        let mut tracker = ClipboardTracker::default();
        let first = entry(1, 5);
        assert!(tracker.unseen(Some(&first)).is_some());
        assert!(tracker.unseen(Some(&first)).is_none());
        assert!(tracker.unseen(Some(&entry(2, 5))).is_some());
        assert!(tracker.unseen(None).is_none());
    }

    #[test]
    fn oversized_entries_are_skipped() {
        let mut tracker = ClipboardTracker::default();
        let huge = entry(
            3,
            u64::try_from(MAX_INLINE_CLIPBOARD_BYTES).unwrap_or(0) + 1,
        );
        assert!(tracker.unseen(Some(&huge)).is_none());
    }

    #[test]
    fn a_peer_clipboard_echo_is_suppressed_once() {
        let mut tracker = ClipboardTracker {
            applied_from_peer: Some(Arc::from(&b"hello"[..])),
            ..ClipboardTracker::default()
        };
        assert!(tracker.is_echo(b"hello"));
        assert!(!tracker.is_echo(b"hello"));
        tracker.applied_from_peer = Some(Arc::from(&b"hello"[..]));
        assert!(!tracker.is_echo(b"other"));
        assert!(tracker.applied_from_peer.is_none());
    }
}
