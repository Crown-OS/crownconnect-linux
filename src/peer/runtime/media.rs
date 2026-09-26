//! Between the sessions and the media pipelines: frames, input and rate changes go out to the
//! pipelines, and what they encode, capture or report comes back onto the wire.

use llts_core::send::EncodedSlice;
use llts_core::session::{Action, Event};
use llts_core::stream::StreamKey;
use llts_node::{AppCommand, MediaRole, OutgoingCommand};
use llts_signaling::device::DeviceId;
use llts_signaling::message::{MuxClass, StreamRef};
use llts_transport::security::StaticKey;
use llts_transport::session::DriverEvent;
use llts_wire::{MediaTimestamp, StreamClass};

use super::event_loop::PeerLoop;
use crate::ipc::server::DaemonEvent;
use crate::peer::convert::{ipc_feature, ipc_id};
use crate::peer::{EncodedFrame, MediaOutput, UnicursorSignal};
use crate::util::error_chain::error_chain;

/// Remote input of every feature travels on the session's first input stream, which both ends
/// open as soon as the session is up.
pub(crate) const INPUT_STREAM: StreamKey = StreamKey::new(StreamClass::Input, 0);

fn stream_key(stream: StreamRef) -> Option<StreamKey> {
    StreamClass::from_byte(stream.class.as_byte()).map(|class| StreamKey::new(class, stream.index))
}

fn slice(frame: &EncodedFrame) -> EncodedSlice<'_> {
    let slice =
        EncodedSlice::new(&frame.bytes, MediaTimestamp::new(frame.timestamp)).ending_frame();
    if frame.keyframe {
        slice.as_keyframe()
    } else {
        slice
    }
}

impl PeerLoop {
    /// Session actions the node does not handle belong to the pipelines; a frame only to a
    /// stream the node negotiated for a decoder.
    pub(super) fn route_to_media(&mut self, event: DriverEvent) {
        let DriverEvent::Session { peer, action } = event else {
            return;
        };
        let peer = DeviceId(peer.0);
        if let Action::PresentFrame { stream, .. } = &action
            && !self.decodes(&peer, *stream)
        {
            tracing::debug!(?stream, "a frame for a stream nothing decodes");
            return;
        }
        self.streams.coordinator().on_session_action(peer, action);
    }

    fn decodes(&self, peer: &DeviceId, stream: StreamKey) -> bool {
        MuxClass::from_byte(stream.class.as_byte())
            .map(|class| StreamRef {
                class,
                index: stream.index,
            })
            .and_then(|stream| self.node.stream_purpose(peer, stream))
            .is_some_and(|(_, role)| role == MediaRole::Decoder)
    }

    pub(super) fn open_input(&mut self, peer: &StaticKey) {
        let Some(session) = self.driver.session_mut(peer) else {
            return;
        };
        if let Err(error) = session.open_input(INPUT_STREAM) {
            tracing::debug!(%error, "input stream");
        }
    }

    /// Puts everything the pipelines queued on the wire.
    pub(super) fn drain_media(&mut self) {
        while let Some(output) = self.streams.coordinator().poll_output() {
            self.on_media_output(output);
        }
        self.settle();
    }

    fn on_media_output(&mut self, output: MediaOutput) {
        match output {
            MediaOutput::Frame {
                peer,
                stream,
                frame,
            } => {
                let Some(stream) = stream_key(stream) else {
                    return;
                };
                self.hand_to_session(
                    peer,
                    &Event::SliceEncoded {
                        stream,
                        slice: slice(&frame),
                    },
                );
            }
            MediaOutput::Input { peer, events } => self.hand_to_session(
                peer,
                &Event::InputQueued {
                    stream: INPUT_STREAM,
                    events: &events,
                },
            ),
            MediaOutput::KeyState { peer, keys } => self.hand_to_session(
                peer,
                &Event::KeyStateUpdated {
                    stream: INPUT_STREAM,
                    state: &keys,
                },
            ),
            MediaOutput::Unicursor { peer, signal } => {
                let command = match signal {
                    UnicursorSignal::Enter(enter) => OutgoingCommand::UnicursorEnter(enter),
                    UnicursorSignal::Leave(leave) => OutgoingCommand::UnicursorLeave(leave),
                };
                if let Err(reason) = self.send_command(peer, command) {
                    tracing::warn!(reason, "cannot hand the cursor over");
                }
            }
            MediaOutput::Failed {
                peer,
                feature,
                reason,
            } => {
                tracing::warn!(%peer, ?feature, reason, "media pipeline failed");
                self.announce(DaemonEvent::FeatureFailed {
                    id: ipc_id(peer),
                    feature: ipc_feature(feature),
                    reason,
                });
                if let Err(reason) = self.run_app(AppCommand::StopFeature { peer, feature }) {
                    tracing::debug!(reason, "the failed feature was already over");
                }
            }
        }
    }

    fn hand_to_session(&mut self, peer: DeviceId, event: &Event<'_>) {
        if let Err(error) = self.driver.handle(&StaticKey(peer.0), event) {
            tracing::debug!(error = %error_chain(&error), "media for a session that is gone");
        }
    }
}
