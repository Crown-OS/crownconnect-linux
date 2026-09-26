//! The IPC schema and llts-signaling describe the same things with separate types, so neither
//! crate depends on the other; these are the crossings.

use llts_signaling::device::DeviceId as SignalingDeviceId;
use llts_signaling::state::{
    CallInfo as SignalingCallInfo, CallStatus as SignalingCallStatus, MediaSession,
    Topic as SignalingTopic,
};

use crate::ipc::proto::{CallInfo, CallStatus, DeviceId, MediaInfo, PlaybackStatus, Topic};

pub(crate) const fn signaling_id(id: DeviceId) -> SignalingDeviceId {
    SignalingDeviceId(id.0)
}

pub(crate) const fn signaling_topic(topic: Topic) -> SignalingTopic {
    match topic {
        Topic::Battery => SignalingTopic::Battery,
        Topic::Media => SignalingTopic::MediaSession,
        Topic::Volume => SignalingTopic::Volume,
        Topic::Clipboard => SignalingTopic::Clipboard,
        Topic::Hotspot => SignalingTopic::Hotspot,
        Topic::Calls => SignalingTopic::CallState,
    }
}

pub(crate) fn media_info(session: &MediaSession<'_>) -> MediaInfo {
    MediaInfo {
        title: session.title.to_owned(),
        artist: session.artist.to_owned(),
        album: String::new(),
        player: session.app.to_owned(),
        status: if session.playing {
            PlaybackStatus::Playing
        } else {
            PlaybackStatus::Paused
        },
        position_ms: session.position_ms,
        duration_ms: session.duration_ms,
    }
}

pub(crate) fn call_info(call: &SignalingCallInfo<'_>) -> CallInfo {
    CallInfo {
        call_id: call.call_id.to_owned(),
        number: call.number.to_owned(),
        contact_name: call.contact_name.map(str::to_owned),
        status: match call.status {
            SignalingCallStatus::Ringing => CallStatus::Ringing,
            SignalingCallStatus::Dialing => CallStatus::Dialing,
            SignalingCallStatus::Active => CallStatus::Active,
            SignalingCallStatus::Held => CallStatus::Held,
            SignalingCallStatus::Ended => CallStatus::Ended,
        },
        answered_unix_ms: call.answered_unix_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ipc_topic_has_a_distinct_signaling_topic() {
        let topics = [
            Topic::Battery,
            Topic::Media,
            Topic::Volume,
            Topic::Clipboard,
            Topic::Hotspot,
            Topic::Calls,
        ];
        let mut mapped: Vec<_> = topics.into_iter().map(signaling_topic).collect();
        mapped.sort_unstable();
        mapped.dedup();
        assert_eq!(mapped.len(), topics.len());
    }

    #[test]
    fn a_paused_session_maps_to_paused_media() {
        let session = MediaSession {
            app: "Spotify",
            title: "Song",
            artist: "Band",
            position_ms: 5,
            duration_ms: 10,
            playing: false,
        };
        let media = media_info(&session);
        assert_eq!(media.status, PlaybackStatus::Paused);
        assert_eq!(media.player, "Spotify");
    }

    #[test]
    fn calls_keep_their_identity_and_status() {
        let call = SignalingCallInfo {
            call_id: "c1",
            number: "+1",
            contact_name: Some("Ada"),
            status: SignalingCallStatus::Held,
            answered_unix_ms: Some(9),
        };
        let mapped = call_info(&call);
        assert_eq!(mapped.status, CallStatus::Held);
        assert_eq!(mapped.contact_name.as_deref(), Some("Ada"));
        assert_eq!(signaling_id(DeviceId([3; 32])).0, [3; 32]);
    }
}
