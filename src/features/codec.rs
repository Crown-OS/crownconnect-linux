use llts_signaling::message::VideoCodec as StreamCodec;

use super::FeatureError;
use crate::media::video::{CodecProfile, HardwareCapabilities, VideoCodec};

pub(crate) const fn media_codec(codec: StreamCodec) -> VideoCodec {
    match codec {
        StreamCodec::Hevc => VideoCodec::Hevc,
        StreamCodec::H264 => VideoCodec::H264,
        StreamCodec::Av1 => VideoCodec::Av1,
    }
}

/// The best 8-bit profile of `codec` this computer can encode.
pub(crate) fn encode_profile(
    codec: StreamCodec,
    capabilities: &HardwareCapabilities,
) -> Result<CodecProfile, FeatureError> {
    let candidates: &[CodecProfile] = match codec {
        StreamCodec::Hevc => &[CodecProfile::HevcMain],
        StreamCodec::H264 => &[
            CodecProfile::H264High,
            CodecProfile::H264ConstrainedBaseline,
        ],
        StreamCodec::Av1 => &[CodecProfile::Av1Main],
    };
    candidates
        .iter()
        .copied()
        .find(|profile| capabilities.codecs.encode.contains(*profile))
        .ok_or(FeatureError::Unsupported(
            "hardware encoding of the negotiated codec",
        ))
}

/// The name `crownconnect-viewer --codec` takes.
pub(crate) const fn viewer_codec(codec: StreamCodec) -> &'static str {
    match codec {
        StreamCodec::Hevc => "hevc",
        StreamCodec::H264 => "h264",
        StreamCodec::Av1 => "av1",
    }
}
