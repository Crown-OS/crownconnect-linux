use ffmpeg_next::ffi::{
    AVColorPrimaries, AVColorRange, AVColorSpace, AVColorTransferCharacteristic, AVPixelFormat,
    AVRational, AV_PROFILE_AV1_MAIN, AV_PROFILE_H264_CONSTRAINED_BASELINE, AV_PROFILE_H264_HIGH,
    AV_PROFILE_HEVC_MAIN, AV_PROFILE_HEVC_MAIN_10,
};
use ffmpeg_next::Dictionary;

use super::{EncoderConfig, RateControl};
use crate::media::video::codec::{BitDepth, CodecProfile, VideoCodec};
use crate::media::video::hw::{BufferRef, CodecContext, CodecRole, EncodeFeatures};
use crate::media::MediaError;

const MICROSECONDS: AVRational = AVRational {
    num: 1,
    den: 1_000_000,
};
const INFINITE_GOP: i32 = i32::MAX;
/// Frames of bitrate the rate control buffer holds; one keeps every frame within its budget.
const RATE_BUFFER_FRAMES: i64 = 1;

pub(super) fn open_context(
    config: &EncoderConfig,
    features: EncodeFeatures,
    frames: &BufferRef,
    bitrate_bps: u32,
) -> Result<CodecContext, MediaError> {
    let name = config.backend.encoder_name(config.profile.codec());
    let mut context = CodecContext::new(name, CodecRole::Encoder)?;
    let frame_rate = i32::try_from(config.frame_rate.max(1)).unwrap_or(60);
    let bitrate = i64::from(bitrate_bps);
    let raw = context.get_mut();
    raw.width = i32::try_from(config.width).unwrap_or_default();
    raw.height = i32::try_from(config.height).unwrap_or_default();
    raw.pix_fmt = AVPixelFormat::AV_PIX_FMT_VAAPI;
    raw.time_base = MICROSECONDS;
    raw.framerate = AVRational {
        num: frame_rate,
        den: 1,
    };
    raw.profile = ffmpeg_profile(config.profile);
    raw.gop_size = INFINITE_GOP;
    raw.max_b_frames = 0;
    raw.bit_rate = bitrate;
    raw.rc_max_rate = match config.rate_control {
        RateControl::Cbr => bitrate,
        RateControl::Vbr => bitrate * 3 / 2,
    };
    raw.rc_buffer_size =
        i32::try_from(bitrate * RATE_BUFFER_FRAMES / i64::from(frame_rate)).unwrap_or(i32::MAX);
    raw.slices = i32::try_from(config.slices.clamp(1, features.max_slices.max(1))).unwrap_or(1);
    raw.hw_frames_ctx = frames.new_ref()?.into_raw();
    raw.colorspace = AVColorSpace::AVCOL_SPC_BT709;
    raw.color_primaries = AVColorPrimaries::AVCOL_PRI_BT709;
    raw.color_trc = AVColorTransferCharacteristic::AVCOL_TRC_BT709;
    raw.color_range = AVColorRange::AVCOL_RANGE_MPEG;
    let rate_control = match config.rate_control {
        RateControl::Cbr => "CBR",
        RateControl::Vbr => "VBR",
    };
    let low_power = if features.low_power_only { "1" } else { "0" };
    let options = [
        ("async_depth", "1"),
        ("rc_mode", rate_control),
        ("low_power", low_power),
        ("aud", "0"),
        ("sei", "0"),
    ];
    context.open(options.into_iter().collect::<Dictionary<'_>>())?;
    Ok(context)
}

pub(super) const fn software_format(profile: CodecProfile) -> AVPixelFormat {
    match profile.bit_depth() {
        BitDepth::Eight => AVPixelFormat::AV_PIX_FMT_NV12,
        BitDepth::Ten => AVPixelFormat::AV_PIX_FMT_P010LE,
    }
}

const fn ffmpeg_profile(profile: CodecProfile) -> i32 {
    match profile {
        CodecProfile::H264ConstrainedBaseline => AV_PROFILE_H264_CONSTRAINED_BASELINE,
        CodecProfile::H264High => AV_PROFILE_H264_HIGH,
        CodecProfile::HevcMain => AV_PROFILE_HEVC_MAIN,
        CodecProfile::HevcMain10 => AV_PROFILE_HEVC_MAIN_10,
        CodecProfile::Av1Main => AV_PROFILE_AV1_MAIN,
    }
}

pub(super) const fn qp_range(codec: VideoCodec) -> i32 {
    match codec {
        VideoCodec::H264 | VideoCodec::Hevc => 51,
        VideoCodec::Av1 => 255,
    }
}
