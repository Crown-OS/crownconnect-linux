//! A peer as an extra monitor: a virtual output in the mode it asked for, rendered only when
//! the encoder is ready for the next frame, with the peer's touch and pointer landing on it.
//! Stopping the pipeline destroys the output.

use std::fmt::Write;

use llts_signaling::device::DeviceId;
use llts_signaling::message::{StartMonitor, VideoParams};

use super::input_route::SharedInjector;
use super::pipeline::PipelineContext;
use super::platform::MediaPlatform;
use super::rate::RatePlanner;
use super::screen::CaptureTarget;
use super::source::{frame_rate, run_encoder};
use super::FeatureError;
use crate::wayland::virtual_output::VirtualOutputMode;

const OUTPUT_NAME_PREFIX: &str = "crownconnect-";
const OUTPUT_NAME_ID_BYTES: usize = 4;

/// A stable output name per peer, within the protocol's alphabet.
pub(crate) fn output_name(peer: &DeviceId) -> String {
    peer.0.iter().take(OUTPUT_NAME_ID_BYTES).fold(
        OUTPUT_NAME_PREFIX.to_owned(),
        |mut name, byte| {
            let _ = write!(name, "{byte:02x}");
            name
        },
    )
}

pub(crate) fn output_mode(monitor: &StartMonitor) -> VirtualOutputMode {
    VirtualOutputMode {
        width: u32::from(monitor.width),
        height: u32::from(monitor.height),
        refresh_mhz: monitor.refresh_mhz,
        scale_120: u32::from(monitor.scale_120),
    }
}

pub(crate) fn extend_desktop(
    context: &PipelineContext,
    platform: &dyn MediaPlatform,
    params: &VideoParams,
    monitor: &StartMonitor,
    input: &SharedInjector,
) -> Result<(), FeatureError> {
    let target = CaptureTarget::VirtualOutput {
        name: output_name(&context.peer),
        mode: output_mode(monitor),
    };
    let mut source = platform.screen_source(target, params)?;
    if let Some(name) = source.output_name() {
        match platform.injector(Some(name.to_owned())) {
            Ok(injector) => {
                let _ = input.set(injector);
            }
            Err(error) => tracing::warn!(%error, "the monitor takes no touch input"),
        }
    }
    let planner = RatePlanner::new(
        params.bitrate_kbps.saturating_mul(1_000),
        frame_rate(params),
    );
    run_encoder(context, &mut *source, planner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wayland::virtual_output::is_valid_output_name;

    #[test]
    fn output_names_are_valid_and_per_peer() {
        let name = output_name(&DeviceId([0xab; 32]));
        assert_eq!(name, "crownconnect-abababab");
        assert!(is_valid_output_name(&name));
    }
}
