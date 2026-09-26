//! Where received media goes, and where a microphone's samples come from.

use std::time::Duration;

use super::pipeline::{Control, PipelineContext, Stopped};
use super::FeatureError;

/// How long a decoder pipeline waits for a frame before checking whether it should stop.
const FRAME_WAIT: Duration = Duration::from_millis(50);

/// One complete frame as the session delivered it.
#[derive(Debug, Clone, Copy)]
pub struct ReceivedFrame<'a> {
    pub bytes: &'a [u8],
    /// Capture time on the stream's clock (90 kHz video, 48 kHz audio).
    pub timestamp: u32,
    pub keyframe: bool,
}

/// Consumes a peer's encoded video: shows it, or turns it into a camera.
pub trait VideoSink: Send {
    /// # Errors
    ///
    /// Fails when the sink can take no more frames, which ends the feature.
    fn present(&mut self, frame: &ReceivedFrame<'_>) -> Result<(), FeatureError>;

    /// Checked between frames; an error ends the feature, as when the viewer was closed.
    ///
    /// # Errors
    ///
    /// Fails once the sink is gone for good.
    fn check(&mut self) -> Result<(), FeatureError> {
        Ok(())
    }
}

/// 10 ms of mono 48 kHz audio at a time, from a microphone or a test tone.
pub trait VoiceSource: Send {
    /// Fills `frame` with the next 10 ms, waiting at most `timeout`; `false` when none was due.
    ///
    /// # Errors
    ///
    /// Fails once capture stopped for good.
    fn read_frame(&mut self, frame: &mut [f32], timeout: Duration) -> Result<bool, FeatureError>;
}

/// Plays decoded mono 48 kHz audio, as a virtual microphone does for other apps.
pub trait VoiceSink: Send {
    /// # Errors
    ///
    /// Fails once the sink is gone for good.
    fn play(&mut self, pcm: &[f32]) -> Result<(), FeatureError>;
}

/// Hands every frame the session assembles to `sink` until stopped.
pub(crate) fn run_decoder(
    context: &PipelineContext,
    sink: &mut dyn VideoSink,
) -> Result<(), FeatureError> {
    loop {
        match context.next_control(FRAME_WAIT) {
            Ok(Some(Control::Frame(frame))) => sink.present(&ReceivedFrame {
                bytes: frame.bytes(),
                timestamp: frame.timestamp().ticks(),
                keyframe: frame.is_keyframe(),
            })?,
            Ok(_) => {}
            Err(Stopped) => return Ok(()),
        }
        sink.check()?;
    }
}
