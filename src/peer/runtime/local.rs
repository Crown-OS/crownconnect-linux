//! This computer's side of state sync: publishing its own documents, and carrying out what
//! peers ask of it through [`LocalControls`](crate::state::LocalControls).

use llts_node::{NodeError, RemoteCommand};
use llts_signaling::device::DeviceId;
use llts_signaling::message::Lock;
use llts_signaling::state::{Clipboard, ClipboardContent, Published};
use tokio::sync::mpsc;

use super::event_loop::PeerLoop;
use crate::state::{ClipboardSnapshot, LocalState, MediaSnapshot};

fn deliver<T>(service: &mpsc::Sender<T>, value: T, what: &'static str) {
    if service.try_send(value).is_err() {
        tracing::warn!(
            service = what,
            "cannot carry out a peer's request: service unavailable"
        );
    }
}

impl PeerLoop {
    pub(super) fn publish(&mut self, mut state: LocalState) {
        let now = self.clock.now();
        if let LocalState::Clipboard(clipboard) = &mut state {
            clipboard.stamp = self.node.clipboard_stamp(now);
        }
        let outcome: Result<Published, NodeError> = match &state {
            LocalState::Battery(battery) => self.node.publish(now, battery),
            LocalState::Volume(volume) => self.node.publish(now, volume),
            LocalState::Media(media) => self
                .node
                .publish(now, &media.as_ref().map(MediaSnapshot::payload)),
            LocalState::Clipboard(clipboard) => clipboard
                .payload()
                .map_err(NodeError::from)
                .and_then(|payload| self.node.publish(now, &payload)),
            LocalState::Hotspot(hotspot) => self.node.publish(now, &hotspot.payload()),
        };
        match outcome {
            Ok(Published::Changed(version)) => {
                tracing::debug!(topic = ?state.topic(), seq = version.seq, "published");
            }
            Ok(Published::Unchanged) => {}
            Err(error) => tracing::warn!(%error, topic = ?state.topic(), "cannot publish"),
        }
    }

    pub(super) fn on_remote_command(&mut self, peer: DeviceId, command: RemoteCommand) {
        let controls = &self.services.controls;
        match command {
            RemoteCommand::Media(media) => deliver(&controls.media, media, "media"),
            RemoteCommand::SetVolume(volume) => deliver(&controls.volume, volume, "volume"),
            RemoteCommand::SetHotspot(hotspot) => deliver(&controls.hotspot, hotspot, "hotspot"),
            RemoteCommand::Lock => deliver(&controls.lock, Lock, "session lock"),
            RemoteCommand::Call(call) => self.on_remote_call(peer, &call),
            RemoteCommand::Reply {
                conversation_id,
                text,
            } => {
                if let Err(reason) = self.send_reply(&conversation_id, &text) {
                    tracing::info!(%peer, reason, "cannot pass a reply on");
                }
            }
            RemoteCommand::FileOffer(offer) => tracing::info!(
                %peer,
                transfer = offer.ffsp_transfer_id.0,
                "a file is on its way over ffsp, which accepts it on its own side"
            ),
            unicursor @ (RemoteCommand::UnicursorEnter(_) | RemoteCommand::UnicursorLeave(_)) => {
                self.streams
                    .coordinator()
                    .on_remote_command(peer, &unicursor);
            }
        }
    }

    /// Makes the newest clipboard this computer's selection, when it was copied on a peer and
    /// has not been applied yet.
    pub(super) fn apply_newest_clipboard(&mut self) {
        let own = self.node.id();
        let Some(snapshot) = self
            .node
            .newest_clipboard()
            .filter(|(origin, clipboard)| {
                *origin != own && self.applied_clipboard != Some(clipboard.stamp)
            })
            .and_then(|(_, clipboard)| inline_snapshot(&clipboard))
        else {
            return;
        };
        self.applied_clipboard = Some(snapshot.stamp);
        deliver(&self.services.controls.clipboard, snapshot, "clipboard");
    }
}

fn inline_snapshot(clipboard: &Clipboard<'_>) -> Option<ClipboardSnapshot> {
    match clipboard.content {
        ClipboardContent::Inline(bytes) => Some(ClipboardSnapshot {
            mime: clipboard.mime.to_owned(),
            bytes: bytes.to_vec(),
            stamp: clipboard.stamp,
        }),
        ClipboardContent::Offer { .. } => {
            tracing::debug!("a large clipboard waits for an ffsp transfer");
            None
        }
    }
}
