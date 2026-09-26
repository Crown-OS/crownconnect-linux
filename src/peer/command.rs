use llts_signaling::message::Media;

use crate::bluetooth::telephony::PhoneCalls;
use crate::discovery::PresenceEvent;
use crate::ipc::proto::{
    CameraOptions, DeviceId, Edge, Feature, FeatureSet, MirrorOptions, MonitorOptions, PairingOffer,
};
use crate::ipc::server::Responder;
use crate::state::LocalState;

/// An IPC request that needs a paired device, with the responder that answers it.
#[derive(Debug)]
pub enum PeerRequest {
    PairingBegin(Responder<PairingOffer>),
    /// Answered once the pairing finished either way.
    PairWithQr {
        qr: String,
        reply: Responder<()>,
    },
    PairingConfirm {
        id: DeviceId,
        accept: bool,
        reply: Responder<()>,
    },
    Forget {
        id: DeviceId,
        reply: Responder<()>,
    },
    SendFile {
        id: DeviceId,
        path: String,
        reply: Responder<()>,
    },
    StartMirror {
        id: DeviceId,
        options: MirrorOptions,
        reply: Responder<()>,
    },
    StartCamera {
        id: DeviceId,
        options: CameraOptions,
        reply: Responder<()>,
    },
    StartMic {
        id: DeviceId,
        reply: Responder<()>,
    },
    StartMonitor {
        id: DeviceId,
        options: MonitorOptions,
        reply: Responder<()>,
    },
    Stop {
        id: DeviceId,
        feature: Feature,
        reply: Responder<()>,
    },
    SetUnicursorEdge {
        id: DeviceId,
        edge: Edge,
        reply: Responder<()>,
    },
    PickupCall {
        call_id: String,
        reply: Responder<()>,
    },
    DeclineCall {
        call_id: String,
        reply: Responder<()>,
    },
    HangupCall {
        call_id: String,
        reply: Responder<()>,
    },
    Dial {
        id: DeviceId,
        number: String,
        reply: Responder<()>,
    },
    SetHotspot {
        id: DeviceId,
        enabled: bool,
        reply: Responder<()>,
    },
}

impl PeerRequest {
    /// Answers the request with a handler error.
    pub fn reject(self, reason: &str) {
        match self {
            Self::PairingBegin(reply) => reply.fail(reason),
            Self::PairWithQr { reply, .. }
            | Self::PairingConfirm { reply, .. }
            | Self::Forget { reply, .. }
            | Self::SendFile { reply, .. }
            | Self::StartMirror { reply, .. }
            | Self::StartCamera { reply, .. }
            | Self::StartMic { reply, .. }
            | Self::StartMonitor { reply, .. }
            | Self::Stop { reply, .. }
            | Self::SetUnicursorEdge { reply, .. }
            | Self::PickupCall { reply, .. }
            | Self::DeclineCall { reply, .. }
            | Self::HangupCall { reply, .. }
            | Self::Dial { reply, .. }
            | Self::SetHotspot { reply, .. } => reply.fail(reason),
        }
    }
}

/// A fire-and-forget IPC notification for the peer runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerNotice {
    PairingCancel,
    /// Drives the media session of whichever device is playing.
    Media(Media),
    SendReply {
        conversation_id: String,
        text: String,
    },
    /// The user changed what a device may use; the runtime persists and enforces it.
    FeaturesChanged {
        id: DeviceId,
        features: FeatureSet,
    },
}

/// Everything the peer runtime is told.
#[derive(Debug)]
pub enum PeerCommand {
    Request(PeerRequest),
    Notice(PeerNotice),
    /// This computer's own state changed and should reach subscribed peers.
    LocalState(LocalState),
    Presence(PresenceEvent),
    Calls(PhoneCalls),
}
