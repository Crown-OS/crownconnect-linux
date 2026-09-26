//! The wire contract of the `crownconnect` service.
//!
//! Lives in the library target, outside the `daemon` feature, so every client
//! — crownotify, crownsettings, any other CrownOS app that talks to a paired
//! device — is generated from the same declaration as the daemon and cannot
//! drift from it.
//!
//! Frozen: a change to any signature changes its selector and breaks every
//! client built against the old one. Add new methods instead.

pub use super::types::*;

crownos_ipc::protocol! {
    pub mod crownconnect: "crownconnect" {
        /// Every device this computer has paired with or can currently see.
        fn devices() -> Vec<DeviceInfo>;
        /// Open a pairing window and return the QR invitation for it. Devices
        /// pairing nearby instead arrive as [`PairingRequest`].
        fn pairing_begin() -> PairingOffer;
        /// Accept or reject the device a [`PairingRequest`] asked about.
        fn pairing_confirm(id: DeviceId, accept: bool);
        /// Close the pairing window and invalidate its QR invitation.
        notify pairing_cancel();
        /// Unpair a device and delete its keys.
        fn forget(id: DeviceId);
        /// Allow or forbid one feature for one device.
        fn set_feature(id: DeviceId, feature: Feature, enabled: bool);
        /// The features the user allows for a device.
        fn features(id: DeviceId) -> FeatureSet;
        /// Offer the file at `path` to a device.
        fn send_file(id: DeviceId, path: String);

        /// Mirror a screen between this computer and a device.
        fn start_mirror(id: DeviceId, options: MirrorOptions);
        /// Use a device's camera as a camera on this computer.
        fn start_camera(id: DeviceId, options: CameraOptions);
        /// Use a device's microphone as a microphone on this computer.
        fn start_mic(id: DeviceId);
        /// Use a device as an extra monitor for this computer.
        fn start_monitor(id: DeviceId, options: MonitorOptions);
        /// Stop whatever `feature` is streaming with a device.
        fn stop(id: DeviceId, feature: Feature);
        /// Hand the pointer and keyboard to a device when the cursor crosses
        /// `edge` of this computer's desktop.
        fn set_unicursor_edge(id: DeviceId, edge: Edge);

        /// Answer an incoming call.
        fn pickup_call(call_id: String);
        /// Decline an incoming call.
        fn decline_call(call_id: String);
        /// End a call in progress.
        fn hangup_call(call_id: String);
        /// Place a call from a device's phone line.
        fn dial(id: DeviceId, number: String);
        /// Resume playback on the device.
        notify media_play();
        /// Pause playback on the device.
        notify media_pause();
        /// Skip to the next track on the device.
        notify media_next();
        /// Go back to the previous track on the device.
        notify media_previous();
        /// Seek to a position in the current track, as a percentage.
        notify media_seek(percent: u8);
        /// Send an inline reply back to the conversation it came from.
        notify send_reply(conversation_id: String, text: String);

        /// The last snapshot of `topic` a device published, postcard-encoded
        /// as the llts-signaling state message for that topic. `None` until
        /// the device has published one.
        fn state(id: DeviceId, topic: Topic) -> Option<Vec<u8>>;
        /// Turn a device's mobile hotspot on or off.
        fn set_hotspot(id: DeviceId, enabled: bool);
        /// Pair with the device showing this QR invitation, as another computer's
        /// [`pairing_begin`] produced it. Phones scan the code instead.
        fn pair_with_qr(qr: String);

        /// A device connected or disconnected.
        event DeviceConnected { id: DeviceId, name: String, connected: bool };
        /// The device list changed; carries the whole new list.
        event DevicesChanged { devices: Vec<DeviceInfo> };
        /// A nearby device wants to pair. Show `code` and answer with
        /// `pairing_confirm` once the user has compared it with the device.
        event PairingRequest { id: DeviceId, name: String, code: String };
        /// A pairing finished, either way.
        event PairingResult { id: DeviceId, accepted: bool };
        /// A call on a device's phone line changed.
        event CallStateChanged { id: DeviceId, call: CallInfo };
        /// What a device is playing changed; `None` when it stopped playing.
        event MediaChanged { id: DeviceId, media: Option<MediaInfo> };
        /// A device's battery changed.
        event BatteryChanged { id: DeviceId, percent: u8, charging: bool };
        /// A device's hotspot was turned on or off.
        event HotspotChanged { id: DeviceId, enabled: bool };
        /// One feature of one device changed state.
        event FeatureChanged { id: DeviceId, feature: Feature, state: FeatureState };
        /// A feature could not start or its pipeline stopped on its own; `reason` says why in
        /// words for the user. A `FeatureChanged` with the resulting state follows.
        event FeatureFailed { id: DeviceId, feature: Feature, reason: String };
    }
}

pub use crownconnect::*;
