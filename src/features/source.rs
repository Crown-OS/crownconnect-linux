//! Where encoded video comes from: a screen capture in the daemon, a test pattern in tests.

use std::time::{Duration, Instant};

use llts_signaling::message::VideoParams;
use llts_wire::{ClockRate, MediaTimestamp};

use super::codec::encode_profile;
use super::pipeline::{Control, PipelineContext, Stopped};
use super::rate::RatePlanner;
use super::FeatureError;
use crate::media::video::{
    EncoderConfig, HardwareCapabilities, Nv12Image, RateControl, VaapiDevice, VideoEncoder,
};
use crate::peer::EncodedFrame;

/// How long an encoder pipeline waits for a frame before looking at its controls again.
const FRAME_WAIT: Duration = Duration::from_millis(20);
const MILLIHERTZ_PER_HERTZ: u32 = 1_000;
const BITS_PER_BYTE: u32 = 8;
/// A keyframe of the synthetic stream is this many times an ordinary frame.
const SYNTHETIC_KEYFRAME_SCALE: usize = 4;
const PATTERN_BLOCK: usize = 16;
const PATTERN_BITS: u32 = 16;
const MID_GREY: u8 = 128;
const BLACK: u8 = 16;
const WHITE: u8 = 235;

/// A source of encoded video frames, polled by the encoder pipeline's thread.
pub trait FrameSource: Send {
    /// The next encoded frame, waiting at most `timeout`.
    ///
    /// # Errors
    ///
    /// Fails when capture or encoding stops for good.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<EncodedFrame>, FeatureError>;

    fn set_target_bitrate(&mut self, bits_per_second: u32);

    fn request_keyframe(&mut self);

    /// The output being captured by name, for input aimed at it.
    fn output_name(&self) -> Option<&str> {
        None
    }
}

/// The frame rate of `params` in whole hertz, at least one.
pub(crate) fn frame_rate(params: &VideoParams) -> u32 {
    params.fps_mhz.div_ceil(MILLIHERTZ_PER_HERTZ).max(1)
}

/// A capture time in microseconds on the stream's 90 kHz clock.
pub(crate) fn video_timestamp(pts_us: i64) -> u32 {
    let micros = u64::try_from(pts_us).unwrap_or_default();
    MediaTimestamp::from_duration(Duration::from_micros(micros), ClockRate::VIDEO).ticks()
}

/// Feeds frames from `source` to the stream until stopped, following the session's rate and
/// keyframe requests.
pub(crate) fn run_encoder(
    context: &PipelineContext,
    source: &mut dyn FrameSource,
    mut planner: RatePlanner,
) -> Result<(), FeatureError> {
    loop {
        loop {
            let control = match context.pending_control() {
                Ok(Some(control)) => control,
                Ok(None) => break,
                Err(Stopped) => return Ok(()),
            };
            let bitrate = match control {
                Control::TargetBitrate(bits) => planner.set_target(bits),
                Control::FrameByteBudget(bytes) => planner.set_frame_budget(bytes),
                Control::Keyframe => {
                    source.request_keyframe();
                    None
                }
                Control::Frame(_) | Control::UnicursorLeave(_) => None,
            };
            if let Some(bits) = bitrate {
                source.set_target_bitrate(bits);
            }
        }
        if let Some(frame) = source.next_frame(FRAME_WAIT)?
            && !context.send_frame(frame)
        {
            source.request_keyframe();
        }
    }
}

/// A moving test pattern at the stream's size and rate: encoded on the GPU when a VA-API
/// device is at hand, otherwise synthetic access units of the size the bitrate allows.
#[derive(Debug)]
pub struct TestPatternSource {
    width: u32,
    height: u32,
    frames_per_second: u32,
    frame_period: Duration,
    bitrate_bps: u32,
    started: Instant,
    next_due: Instant,
    frames: u64,
    keyframe_requested: bool,
    hardware: Option<PatternEncoder>,
}

#[derive(Debug)]
struct PatternEncoder {
    encoder: VideoEncoder,
    luma: Vec<u8>,
    chroma: Vec<u8>,
}

impl TestPatternSource {
    /// Synthetic access units, with no hardware involved.
    pub fn synthetic(params: &VideoParams) -> Self {
        let frames_per_second = frame_rate(params);
        let now = Instant::now();
        Self {
            width: u32::from(params.width),
            height: u32::from(params.height),
            frames_per_second,
            frame_period: Duration::from_secs(1) / frames_per_second,
            bitrate_bps: params.bitrate_kbps.saturating_mul(1_000),
            started: now,
            next_due: now,
            frames: 0,
            keyframe_requested: true,
            hardware: None,
        }
    }

    /// The pattern encoded with the negotiated codec on `device`.
    ///
    /// # Errors
    ///
    /// Fails without a VA-API encoder for the stream's codec.
    pub fn encoded(params: &VideoParams, device: &VaapiDevice) -> Result<Self, FeatureError> {
        let capabilities = HardwareCapabilities::probe(device)?;
        let mut source = Self::synthetic(params);
        let encoder = VideoEncoder::new(
            device,
            &capabilities,
            EncoderConfig {
                profile: encode_profile(params.codec, &capabilities)?,
                width: source.width,
                height: source.height,
                frame_rate: source.frames_per_second,
                bitrate_bps: source.bitrate_bps,
                rate_control: RateControl::Cbr,
                slices: 1,
                backend: crate::media::video::EncoderBackend::default(),
            },
        )?;
        let pixels = source.width as usize * source.height as usize;
        source.hardware = Some(PatternEncoder {
            encoder,
            luma: vec![MID_GREY; pixels],
            chroma: vec![MID_GREY; pixels / 2],
        });
        Ok(source)
    }

    fn synthetic_frame(&mut self, timestamp: u32) -> EncodedFrame {
        let bytes = (self.bitrate_bps / BITS_PER_BYTE / self.frames_per_second).max(1) as usize;
        let keyframe = std::mem::take(&mut self.keyframe_requested);
        let len = if keyframe {
            bytes * SYNTHETIC_KEYFRAME_SCALE
        } else {
            bytes
        };
        EncodedFrame {
            bytes: self
                .frames
                .to_le_bytes()
                .into_iter()
                .cycle()
                .take(len)
                .collect(),
            timestamp,
            keyframe,
        }
    }
}

impl PatternEncoder {
    /// Draws `counter` as black and white blocks along the top rows.
    fn draw(&mut self, width: u32, counter: u64) {
        let width = width as usize;
        for bit in 0..PATTERN_BITS {
            let value = if (counter >> bit) & 1 == 1 {
                WHITE
            } else {
                BLACK
            };
            let left = bit as usize * PATTERN_BLOCK;
            for row in self.luma.chunks_mut(width).take(PATTERN_BLOCK) {
                if let Some(block) = row.get_mut(left..left + PATTERN_BLOCK) {
                    block.fill(value);
                }
            }
        }
    }

    fn encode(
        &mut self,
        width: u32,
        height: u32,
        counter: u64,
        pts_us: i64,
    ) -> Result<Option<EncodedFrame>, FeatureError> {
        self.draw(width, counter);
        self.encoder.encode_nv12(
            &Nv12Image {
                width,
                height,
                luma: &self.luma,
                luma_stride: width as usize,
                chroma: &self.chroma,
                chroma_stride: width as usize,
            },
            pts_us,
        )?;
        Ok(self.encoder.next_unit()?.map(|unit| EncodedFrame {
            bytes: unit.data.to_vec(),
            timestamp: video_timestamp(unit.pts),
            keyframe: unit.keyframe,
        }))
    }
}

impl FrameSource for TestPatternSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<EncodedFrame>, FeatureError> {
        let wait = self.next_due.saturating_duration_since(Instant::now());
        if wait > timeout {
            std::thread::sleep(timeout);
            return Ok(None);
        }
        std::thread::sleep(wait);
        self.next_due += self.frame_period;
        let pts_us = i64::try_from(self.started.elapsed().as_micros()).unwrap_or(i64::MAX);
        let counter = self.frames;
        self.frames += 1;
        let (width, height) = (self.width, self.height);
        match &mut self.hardware {
            Some(pattern) => pattern.encode(width, height, counter, pts_us),
            None => Ok(Some(self.synthetic_frame(video_timestamp(pts_us)))),
        }
    }

    fn set_target_bitrate(&mut self, bits_per_second: u32) {
        self.bitrate_bps = bits_per_second;
        if let Some(pattern) = &mut self.hardware {
            pattern.encoder.set_target_bitrate(bits_per_second);
        }
    }

    fn request_keyframe(&mut self) {
        self.keyframe_requested = true;
        if let Some(pattern) = &mut self.hardware {
            pattern.encoder.request_keyframe();
        }
    }
}
