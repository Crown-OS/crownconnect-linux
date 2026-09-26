//! This computer's screen as a video stream: a crownos screencast ring encoded in place on the
//! GPU, each slot handed back once its packet is out. A virtual output is captured the same way,
//! rendered only when the encoder is ready for the next frame.

use std::collections::VecDeque;
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use llts_signaling::message::VideoParams;

use super::codec::encode_profile;
use super::source::{frame_rate, video_timestamp, FrameSource};
use super::FeatureError;
use crate::media::video::{
    CodecProfile, EncoderBackend, EncoderConfig, HardwareCapabilities, RateControl, VaapiDevice,
    VideoEncoder,
};
use crate::peer::EncodedFrame;
use crate::wayland::channel_outlet;
use crate::wayland::screencast::{
    CaptureRing, CapturedFrame, CursorDelivery, Screencast, ScreencastEvent, ScreencastOptions,
    StopCause, DEFAULT_RING_SLOTS,
};
use crate::wayland::virtual_output::{VirtualOutput, VirtualOutputEvent, VirtualOutputMode};

const SCREEN_EVENT_DEPTH: usize = 16;
const OUTPUT_EVENT_DEPTH: usize = 4;
const OUTPUT_CREATION_TIMEOUT: Duration = Duration::from_secs(3);

/// Which screen to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    PrimaryOutput,
    /// A new output with no display behind it, destroyed when the capture ends.
    VirtualOutput {
        name: String,
        mode: VirtualOutputMode,
    },
}

/// A virtual output that renders a frame each time the encoder asks for one.
#[derive(Debug)]
struct PacedOutput {
    output: VirtualOutput,
    events: Receiver<VirtualOutputEvent>,
    name: String,
}

impl PacedOutput {
    fn create(name: &str, mode: VirtualOutputMode) -> Result<Self, FeatureError> {
        let (sender, events) = sync_channel(OUTPUT_EVENT_DEPTH);
        let output = VirtualOutput::create(name, mode, channel_outlet(sender))?;
        match events.recv_timeout(OUTPUT_CREATION_TIMEOUT) {
            Ok(VirtualOutputEvent::Created { name }) => Ok(Self {
                output,
                events,
                name,
            }),
            Ok(VirtualOutputEvent::Closed(cause)) => Err(FeatureError::OutputClosed(cause)),
            Err(_) => Err(FeatureError::OutputTimedOut),
        }
    }

    fn check(&self) -> Result<(), FeatureError> {
        match self.events.try_recv() {
            Ok(VirtualOutputEvent::Closed(cause)) => Err(FeatureError::OutputClosed(cause)),
            Ok(VirtualOutputEvent::Created { .. }) | Err(TryRecvError::Empty) => Ok(()),
            Err(TryRecvError::Disconnected) => Err(FeatureError::OutputTimedOut),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ScreenSource {
    device: VaapiDevice,
    capabilities: HardwareCapabilities,
    profile: CodecProfile,
    frames_per_second: u32,
    bitrate_bps: u32,
    encoder: Option<VideoEncoder>,
    /// Frames submitted to the encoder whose packets are not out yet, oldest first.
    in_flight: VecDeque<CapturedFrame>,
    ready: VecDeque<EncodedFrame>,
    events: Receiver<ScreencastEvent>,
    screencast: Screencast,
    paced: Option<PacedOutput>,
}

impl ScreenSource {
    pub(crate) fn open(target: CaptureTarget, params: &VideoParams) -> Result<Self, FeatureError> {
        let device = VaapiDevice::open_default()?;
        let capabilities = HardwareCapabilities::probe(&device)?;
        let profile = encode_profile(params.codec, &capabilities)?;
        let paced = match target {
            CaptureTarget::PrimaryOutput => None,
            CaptureTarget::VirtualOutput { name, mode } => Some(PacedOutput::create(&name, mode)?),
        };
        let frames_per_second = frame_rate(params);
        let (sender, events) = sync_channel(SCREEN_EVENT_DEPTH);
        let screencast = Screencast::start(
            ScreencastOptions {
                output: paced.as_ref().map(|paced| paced.name.clone()),
                cursor: CursorDelivery::Embedded,
                max_fps: frames_per_second,
                ring_slots: DEFAULT_RING_SLOTS,
            },
            channel_outlet(sender),
        )?;
        if let Some(paced) = &paced {
            paced.output.request_frame()?;
        }
        Ok(Self {
            device,
            capabilities,
            profile,
            frames_per_second,
            bitrate_bps: params.bitrate_kbps.saturating_mul(1_000),
            encoder: None,
            in_flight: VecDeque::new(),
            ready: VecDeque::new(),
            events,
            screencast,
            paced,
        })
    }

    fn adopt_ring(&mut self, ring: &CaptureRing) {
        match &mut self.encoder {
            Some(encoder)
                if (encoder.config().width, encoder.config().height)
                    == (ring.width, ring.height) =>
            {
                encoder.forget_ring();
            }
            _ => self.encoder = None,
        }
    }

    fn encoder_for(&mut self, width: u32, height: u32) -> Result<&mut VideoEncoder, FeatureError> {
        let encoder = match self.encoder.take() {
            Some(encoder) => encoder,
            None => VideoEncoder::new(
                &self.device,
                &self.capabilities,
                EncoderConfig {
                    profile: self.profile,
                    width,
                    height,
                    frame_rate: self.frames_per_second,
                    bitrate_bps: self.bitrate_bps,
                    rate_control: RateControl::Cbr,
                    slices: 1,
                    backend: EncoderBackend::default(),
                },
            )?,
        };
        Ok(self.encoder.insert(encoder))
    }

    /// Encodes `frame` straight from its dmabuf and gives back every slot whose packet is out.
    fn encode(&mut self, frame: CapturedFrame) -> Result<(), FeatureError> {
        if let Err(error) = frame.wait_rendered() {
            tracing::debug!(%error, "dropping a frame that never finished rendering");
            return Ok(self.screencast.release(frame)?);
        }
        let pts_us = i64::try_from(frame.pts.as_micros()).unwrap_or(i64::MAX);
        let submitted = match frame.dmabuf() {
            Some(dmabuf) => {
                let encoder = self.encoder_for(dmabuf.width, dmabuf.height)?;
                encoder.encode_dmabuf(frame.slot, &dmabuf, pts_us)
            }
            None => Ok(()),
        };
        self.in_flight.push_back(frame);
        submitted?;
        let Some(encoder) = self.encoder.as_mut() else {
            return Ok(());
        };
        while let Some(unit) = encoder.next_unit()? {
            self.ready.push_back(EncodedFrame {
                bytes: unit.data.to_vec(),
                timestamp: video_timestamp(unit.pts),
                keyframe: unit.keyframe,
            });
            if let Some(done) = self.in_flight.pop_front() {
                self.screencast.release(done)?;
            }
        }
        if let Some(paced) = &self.paced
            && !self.ready.is_empty()
        {
            paced.output.request_frame()?;
        }
        Ok(())
    }
}

impl FrameSource for ScreenSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<EncodedFrame>, FeatureError> {
        if let Some(paced) = &self.paced {
            paced.check()?;
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.ready.pop_front() {
                return Ok(Some(frame));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(remaining) {
                Ok(ScreencastEvent::Ring(ring)) => self.adopt_ring(&ring),
                Ok(ScreencastEvent::Frame(frame)) => self.encode(frame)?,
                Ok(ScreencastEvent::Cursor(_)) => {}
                Ok(ScreencastEvent::Stopped(cause)) => {
                    return Err(FeatureError::CaptureStopped(cause));
                }
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(FeatureError::CaptureStopped(StopCause::Requested));
                }
            }
        }
    }

    fn set_target_bitrate(&mut self, bits_per_second: u32) {
        self.bitrate_bps = bits_per_second;
        if let Some(encoder) = &mut self.encoder {
            encoder.set_target_bitrate(bits_per_second);
        }
    }

    fn request_keyframe(&mut self) {
        if let Some(encoder) = &mut self.encoder {
            encoder.request_keyframe();
        }
        let refreshed = match &self.paced {
            Some(paced) => paced.output.request_frame(),
            None => self.screencast.force_frame(),
        };
        if let Err(error) = refreshed {
            tracing::debug!(%error, "cannot ask for a fresh frame");
        }
    }

    fn output_name(&self) -> Option<&str> {
        self.paced.as_ref().map(|paced| paced.name.as_str())
    }
}
