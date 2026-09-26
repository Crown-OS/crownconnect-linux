//! Screen mirroring. As the encoder this computer's primary output streams to the peer, whose
//! input is injected back through the route the coordinator keeps; as the decoder the peer's
//! screen opens in a viewer window whose input goes to the peer.

use llts_signaling::message::VideoParams;

use super::pipeline::PipelineContext;
use super::platform::{InputForwarder, MediaPlatform, MirrorWindow};
use super::rate::RatePlanner;
use super::screen::CaptureTarget;
use super::sink::run_decoder;
use super::source::{frame_rate, run_encoder};
use super::FeatureError;

pub(crate) fn share_screen(
    context: &PipelineContext,
    platform: &dyn MediaPlatform,
    params: &VideoParams,
) -> Result<(), FeatureError> {
    let mut source = platform.screen_source(CaptureTarget::PrimaryOutput, params)?;
    let planner = RatePlanner::new(
        params.bitrate_kbps.saturating_mul(1_000),
        frame_rate(params),
    );
    run_encoder(context, &mut *source, planner)
}

pub(crate) fn show_peer_screen(
    context: &PipelineContext,
    platform: &dyn MediaPlatform,
    window: MirrorWindow<'_>,
    input: Option<InputForwarder>,
) -> Result<(), FeatureError> {
    let mut sink = platform.mirror_sink(window, input)?;
    run_decoder(context, &mut *sink)
}
