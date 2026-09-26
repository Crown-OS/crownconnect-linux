use crownos_ipc::Server;

use crate::ipc::proto;

/// An IPC event waiting to be broadcast to its subscribers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OutgoingEvent {
    DeviceConnected(proto::DeviceConnected),
    DevicesChanged(proto::DevicesChanged),
    PairingRequest(proto::PairingRequest),
    PairingResult(proto::PairingResult),
    CallStateChanged(proto::CallStateChanged),
    MediaChanged(proto::MediaChanged),
    BatteryChanged(proto::BatteryChanged),
    HotspotChanged(proto::HotspotChanged),
    FeatureChanged(proto::FeatureChanged),
    FeatureFailed(proto::FeatureFailed),
}

impl OutgoingEvent {
    pub(crate) fn emit(&self, server: &mut Server) -> Result<(), crownos_ipc::Error> {
        match self {
            Self::DeviceConnected(event) => server.emit(event),
            Self::DevicesChanged(event) => server.emit(event),
            Self::PairingRequest(event) => server.emit(event),
            Self::PairingResult(event) => server.emit(event),
            Self::CallStateChanged(event) => server.emit(event),
            Self::MediaChanged(event) => server.emit(event),
            Self::BatteryChanged(event) => server.emit(event),
            Self::HotspotChanged(event) => server.emit(event),
            Self::FeatureChanged(event) => server.emit(event),
            Self::FeatureFailed(event) => server.emit(event),
        }
    }
}
