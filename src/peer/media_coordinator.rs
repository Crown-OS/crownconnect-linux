//! Where the peer runtime hands the media pipelines it negotiated, the session actions meant for
//! them, and what they produce in return.

use llts_core::session::Action;
use llts_node::{MediaRequest, RemoteCommand};
use llts_signaling::device::{DeviceId, Feature};
use llts_signaling::message::{StreamRef, UnicursorEnter, UnicursorLeave};

use crate::ipc::proto::Edge;
use crate::peer::wakeup::RuntimeWaker;

const UNICURSOR_UNAVAILABLE: &str = "unicursor is not available on this computer";

/// One encoded frame or Opus packet, owned so it can cross from the encoder's thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub bytes: Vec<u8>,
    /// Capture time in the stream's clock: 90 kHz for video, 48 kHz for audio.
    pub timestamp: u32,
    pub keyframe: bool,
}

/// A shared-cursor handover to tell a peer about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnicursorSignal {
    Enter(UnicursorEnter),
    Leave(UnicursorLeave),
}

/// Something a pipeline needs the runtime to put on the wire or report.
#[derive(Debug)]
pub enum MediaOutput {
    Frame {
        peer: DeviceId,
        stream: StreamRef,
        frame: EncodedFrame,
    },
    /// One batch of input events for the peer's input stream.
    Input { peer: DeviceId, events: Vec<u8> },
    /// Every key this side holds down, replacing what the peer believes.
    KeyState { peer: DeviceId, keys: Vec<u8> },
    Unicursor {
        peer: DeviceId,
        signal: UnicursorSignal,
    },
    /// The pipeline behind `feature` stopped on its own; the runtime ends the feature on both
    /// ends and reports `reason`.
    Failed {
        peer: DeviceId,
        feature: Feature,
        reason: String,
    },
}

/// Starts and stops the local encoder or decoder of each negotiated stream. The runtime calls
/// it from its own thread and must not be kept waiting, so pipelines run elsewhere and hand
/// back [`MediaOutput`]s, ringing the [`RuntimeWaker`] when they do.
pub trait MediaCoordinator: Send + std::fmt::Debug {
    /// Called once, on the runtime thread, before any other method.
    fn attach(&mut self, _waker: RuntimeWaker) {}

    fn handle(&mut self, request: &MediaRequest, peer_name: &str);

    /// Frames, input and encoder rate changes from a peer's session.
    fn on_session_action(&mut self, _peer: DeviceId, _action: Action) {}

    fn on_connection(&mut self, _peer: DeviceId, _connected: bool) {}

    /// Shared-cursor handovers a peer sent.
    fn on_remote_command(&mut self, _peer: DeviceId, _command: &RemoteCommand) {}

    /// Hands the pointer and keyboard to `peer` when the cursor crosses `edge`.
    ///
    /// # Errors
    ///
    /// Fails with a reason for the user when input capture is unavailable.
    fn set_unicursor_edge(&mut self, _peer: DeviceId, _edge: Edge) -> Result<(), String> {
        Err(UNICURSOR_UNAVAILABLE.to_owned())
    }

    fn poll_output(&mut self) -> Option<MediaOutput> {
        None
    }
}

/// Accepts every stream and runs nothing, for builds without media pipelines.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoggingMediaCoordinator;

impl MediaCoordinator for LoggingMediaCoordinator {
    fn handle(&mut self, request: &MediaRequest, _peer_name: &str) {
        tracing::info!(?request, "no media pipeline to run this stream");
    }
}

/// Forwards every request to whoever owns the pipelines.
impl MediaCoordinator for std::sync::mpsc::Sender<MediaRequest> {
    fn handle(&mut self, request: &MediaRequest, _peer_name: &str) {
        if self.send(*request).is_err() {
            tracing::warn!(?request, "the media pipelines have stopped");
        }
    }
}
