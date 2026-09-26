//! Media features end to end between two full daemons on loopback: each runs the real feature
//! coordinator, with a test pattern in place of the screen, a recording sink in place of the
//! viewer, and a tone in place of the microphone.
//!
//! Protocol latency is measured per frame from the moment the encoder handed the access unit
//! over to the moment the peer's decoder pipeline received it, which covers both runtimes, the
//! encrypted llts sessions, pacing, loss repair and reassembly.

#![cfg(feature = "daemon")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "a test that fails a precondition should fail loudly"
)]

mod support;

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crownconnect_linux::features::{
    CaptureTarget, FeatureCoordinator, FeatureError, FrameSource, InputForwarder, MediaPlatform,
    MirrorWindow, ReceivedFrame, TestPatternSource, VideoSink, VoiceSink, VoiceSource,
};
use crownconnect_linux::ipc::proto::{
    self, DeviceId, Direction, Feature, FeatureState, MirrorOptions, VideoLimits,
};
use crownconnect_linux::media::video::{
    CodecProfile, HardwareCapabilities, VaapiDevice, VideoCodec, VideoDecoder,
};
use crownconnect_linux::peer::EncodedFrame;
use crownconnect_linux::util::latency::{LatencySummary, LatencyWindow};
use crownconnect_linux::wayland::injector::Injector;
use crownconnect_linux::wayland::input_capture::{CaptureEvent, InputCapture};
use crownconnect_linux::wayland::EventOutlet;
use llts_signaling::message::{VideoCodec as StreamCodec, VideoParams};
use support::{pair_by_qr, Daemon, Home, PATIENCE, REPLY};

const WIDTH: u16 = 1280;
const HEIGHT: u16 = 720;
const FPS: u16 = 60;
/// Frames while congestion control finds the link, left out of the figures.
const WARMUP_FRAMES: usize = 60;
const MEASURED_FRAMES: usize = 360;
const P99_TARGET: Duration = Duration::from_millis(12);
const TONE_HZ: f32 = 440.0;
const TONE_AMPLITUDE: f32 = 0.5;
const SAMPLE_RATE: f32 = 48_000.0;
const FRAME_PERIOD: Duration = Duration::from_millis(10);
const TONE_SECONDS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoMode {
    Synthetic,
    Vaapi,
}

#[derive(Debug)]
struct Arrival {
    timestamp: u32,
    at: Instant,
    bytes: usize,
    decode: Option<Duration>,
}

/// What both daemons' test devices report to the test.
#[derive(Debug)]
struct Probe {
    mode: Mutex<VideoMode>,
    sent: Mutex<HashMap<u32, Instant>>,
    arrivals: Mutex<std_mpsc::Sender<Arrival>>,
    voice: Mutex<Vec<f32>>,
}

#[derive(Debug)]
struct TestPlatform {
    probe: Arc<Probe>,
}

const fn media_codec(codec: StreamCodec) -> VideoCodec {
    match codec {
        StreamCodec::Hevc => VideoCodec::Hevc,
        StreamCodec::H264 => VideoCodec::H264,
        StreamCodec::Av1 => VideoCodec::Av1,
    }
}

impl MediaPlatform for TestPlatform {
    fn screen_source(
        &self,
        _target: CaptureTarget,
        params: &VideoParams,
    ) -> Result<Box<dyn FrameSource>, FeatureError> {
        let mode = *self.probe.mode.lock().expect("mode");
        let pattern = match mode {
            VideoMode::Synthetic => TestPatternSource::synthetic(params),
            VideoMode::Vaapi => TestPatternSource::encoded(params, &VaapiDevice::open_default()?)?,
        };
        Ok(Box::new(RecordingSource {
            pattern,
            probe: Arc::clone(&self.probe),
        }))
    }

    fn mirror_sink(
        &self,
        window: MirrorWindow<'_>,
        _input: Option<InputForwarder>,
    ) -> Result<Box<dyn VideoSink>, FeatureError> {
        let decoder = match *self.probe.mode.lock().expect("mode") {
            VideoMode::Synthetic => None,
            VideoMode::Vaapi => {
                let device = VaapiDevice::open_default()?;
                let decoder = VideoDecoder::new(&device, media_codec(window.params.codec))?;
                Some((decoder, device))
            }
        };
        Ok(Box::new(RecordingSink {
            probe: Arc::clone(&self.probe),
            decoder,
        }))
    }

    fn camera_sink(&self, _params: &VideoParams) -> Result<Box<dyn VideoSink>, FeatureError> {
        Err(FeatureError::Unsupported("a camera in this test"))
    }

    fn voice_source(&self) -> Result<Box<dyn VoiceSource>, FeatureError> {
        Ok(Box::new(ToneSource {
            phase: 0.0,
            next_due: Instant::now(),
        }))
    }

    fn voice_sink(&self, _peer_name: &str) -> Result<Box<dyn VoiceSink>, FeatureError> {
        Ok(Box::new(RecordingVoice {
            probe: Arc::clone(&self.probe),
        }))
    }

    fn injector(&self, _output: Option<String>) -> Result<Injector, FeatureError> {
        Err(FeatureError::Unsupported("input injection in this test"))
    }

    fn input_capture(
        &self,
        _events: EventOutlet<CaptureEvent>,
    ) -> Result<InputCapture, FeatureError> {
        Err(FeatureError::Unsupported("input capture in this test"))
    }
}

/// Stamps each access unit as the encoder hands it over.
struct RecordingSource {
    pattern: TestPatternSource,
    probe: Arc<Probe>,
}

impl FrameSource for RecordingSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<EncodedFrame>, FeatureError> {
        let frame = self.pattern.next_frame(timeout)?;
        if let Some(frame) = &frame {
            self.probe
                .sent
                .lock()
                .expect("sent")
                .insert(frame.timestamp, Instant::now());
        }
        Ok(frame)
    }

    fn set_target_bitrate(&mut self, bits_per_second: u32) {
        self.pattern.set_target_bitrate(bits_per_second);
    }

    fn request_keyframe(&mut self) {
        self.pattern.request_keyframe();
    }
}

/// Stamps each access unit as the decoder pipeline receives it, then decodes it on the GPU in
/// the hardware run.
struct RecordingSink {
    probe: Arc<Probe>,
    decoder: Option<(VideoDecoder, VaapiDevice)>,
}

impl VideoSink for RecordingSink {
    fn present(&mut self, frame: &ReceivedFrame<'_>) -> Result<(), FeatureError> {
        let at = Instant::now();
        let decode = match &mut self.decoder {
            Some((decoder, _)) => {
                decoder.decode(frame.bytes, i64::from(frame.timestamp))?;
                let mut decoded = false;
                while decoder.next_frame()?.is_some() {
                    decoded = true;
                }
                decoded.then(|| at.elapsed())
            }
            None => None,
        };
        let _ = self.probe.arrivals.lock().expect("arrivals").send(Arrival {
            timestamp: frame.timestamp,
            at,
            bytes: frame.bytes.len(),
            decode,
        });
        Ok(())
    }
}

struct ToneSource {
    phase: f32,
    next_due: Instant,
}

impl VoiceSource for ToneSource {
    fn read_frame(&mut self, frame: &mut [f32], timeout: Duration) -> Result<bool, FeatureError> {
        let wait = self.next_due.saturating_duration_since(Instant::now());
        if wait > timeout {
            std::thread::sleep(timeout);
            return Ok(false);
        }
        std::thread::sleep(wait);
        self.next_due += FRAME_PERIOD;
        let step = std::f32::consts::TAU * TONE_HZ / SAMPLE_RATE;
        for sample in frame.iter_mut() {
            *sample = TONE_AMPLITUDE * self.phase.sin();
            self.phase = (self.phase + step) % std::f32::consts::TAU;
        }
        Ok(true)
    }
}

struct RecordingVoice {
    probe: Arc<Probe>,
}

impl VoiceSink for RecordingVoice {
    fn play(&mut self, pcm: &[f32]) -> Result<(), FeatureError> {
        self.probe
            .voice
            .lock()
            .expect("voice")
            .extend_from_slice(pcm);
        Ok(())
    }
}

fn coordinator(probe: &Arc<Probe>) -> Box<FeatureCoordinator> {
    Box::new(FeatureCoordinator::new(Arc::new(TestPlatform {
        probe: Arc::clone(probe),
    })))
}

fn vaapi_available() -> bool {
    if !Path::new("/dev/dri/renderD128").exists() {
        return false;
    }
    let Ok(device) = VaapiDevice::open_default() else {
        return false;
    };
    HardwareCapabilities::probe(&device).is_ok_and(|capabilities| {
        capabilities.codecs.encode.contains(CodecProfile::HevcMain)
            && capabilities.codecs.decode.contains(CodecProfile::HevcMain)
    })
}

fn feature_is(event: &proto::Event, feature: Feature, state: FeatureState) -> bool {
    matches!(event, proto::Event::FeatureChanged(changed)
        if changed.feature == feature && changed.state == state)
}

/// Starts `feature` and waits until it runs.
fn start(
    daemon: &mut Daemon,
    feature: Feature,
    request: impl FnOnce(&mut proto::Client) -> crownos_ipc::Pending<()>,
) {
    let pending = request(&mut daemon.client);
    daemon
        .client
        .wait(pending, REPLY)
        .expect("the start was taken");
    let outcome = daemon.next_event("the feature starting", |event| {
        feature_is(event, feature, FeatureState::Active)
            || matches!(event, proto::Event::FeatureFailed(failed) if failed.feature == feature)
    });
    if let proto::Event::FeatureFailed(failed) = outcome {
        panic!("{feature:?} failed: {}", failed.reason);
    }
}

#[derive(Debug)]
struct MirrorRun {
    frames: usize,
    protocol: LatencySummary,
    decode: Option<LatencySummary>,
    mean_bytes: usize,
}

/// Mirrors the encoder daemon's test screen to the decoder daemon and measures every frame
/// after the warm-up.
fn mirror(
    encoder: &mut Daemon,
    decoder_id: DeviceId,
    probe: &Probe,
    arrivals: &std_mpsc::Receiver<Arrival>,
    mode: VideoMode,
) -> MirrorRun {
    *probe.mode.lock().expect("mode") = mode;
    probe.sent.lock().expect("sent").clear();
    while arrivals.try_recv().is_ok() {}
    let options = MirrorOptions {
        direction: Direction::ToPeer,
        limits: VideoLimits {
            max_width: WIDTH,
            max_height: HEIGHT,
            max_fps: FPS,
        },
        remote_input: false,
    };
    start(encoder, Feature::Mirror, |client| {
        client
            .start_mirror(decoder_id, options)
            .expect("start_mirror")
    });

    let mut protocol = LatencyWindow::with_capacity(MEASURED_FRAMES);
    let mut decode = LatencyWindow::with_capacity(MEASURED_FRAMES);
    let mut decoded = 0;
    let mut bytes = 0;
    let mut frames = 0;
    let deadline = Instant::now() + PATIENCE;
    while frames < WARMUP_FRAMES + MEASURED_FRAMES {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let arrival = arrivals
            .recv_timeout(remaining)
            .unwrap_or_else(|_| panic!("only {frames} frames arrived"));
        frames += 1;
        if frames <= WARMUP_FRAMES {
            continue;
        }
        let sent = probe
            .sent
            .lock()
            .expect("sent")
            .get(&arrival.timestamp)
            .copied()
            .expect("every frame that arrives was sent");
        protocol.record(arrival.at.duration_since(sent));
        bytes += arrival.bytes;
        if let Some(took) = arrival.decode {
            decode.record(took);
            decoded += 1;
        }
    }

    let pending = encoder
        .client
        .stop(decoder_id, Feature::Mirror)
        .expect("stop");
    encoder.client.wait(pending, REPLY).expect("mirror stopped");
    encoder.next_event("the mirror stopped", |event| {
        feature_is(event, Feature::Mirror, FeatureState::Enabled)
    });
    if mode == VideoMode::Vaapi {
        assert!(
            decoded >= MEASURED_FRAMES * 9 / 10,
            "only {decoded} of {MEASURED_FRAMES} frames decoded"
        );
    }
    MirrorRun {
        frames: MEASURED_FRAMES,
        protocol: protocol.summary().expect("latencies"),
        decode: decode.summary(),
        mean_bytes: bytes / MEASURED_FRAMES,
    }
}

/// Zero crossings per second of the recorded tone's last second.
fn tone_frequency(samples: &[f32]) -> f32 {
    let second = &samples[samples.len() - 48_000..];
    let crossings = second
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count();
    crossings as f32 / 2.0
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Streams the decoder daemon's test tone to the encoder daemon's virtual microphone.
fn microphone(listener: &mut Daemon, speaker_id: DeviceId, probe: &Probe) -> (f32, f32) {
    probe.voice.lock().expect("voice").clear();
    start(listener, Feature::Mic, |client| {
        client.start_mic(speaker_id).expect("start_mic")
    });
    let wanted = TONE_SECONDS * 48_000;
    let deadline = Instant::now() + PATIENCE;
    while probe.voice.lock().expect("voice").len() < wanted {
        assert!(Instant::now() < deadline, "the tone never arrived");
        std::thread::sleep(FRAME_PERIOD);
    }
    let pending = listener
        .client
        .stop(speaker_id, Feature::Mic)
        .expect("stop mic");
    listener.client.wait(pending, REPLY).expect("mic stopped");
    let voice = probe.voice.lock().expect("voice");
    (tone_frequency(&voice), rms(&voice[voice.len() - 48_000..]))
}

fn describe(run: &MirrorRun) -> String {
    let decode = run.decode.map_or_else(String::new, |decode| {
        format!(
            ", GPU decode median {:?} p99 {:?}",
            decode.median, decode.p99
        )
    });
    format!(
        "{} frames of {WIDTH}x{HEIGHT}@{FPS} averaging {} B: protocol latency median {:?} p99 {:?}{decode}",
        run.frames, run.mean_bytes, run.protocol.median, run.protocol.p99
    )
}

#[test]
fn mirror_and_mic_stream_between_two_daemons_on_loopback() {
    support::init_logging();
    let (arrivals_sender, arrivals) = std_mpsc::channel();
    let probe = Arc::new(Probe {
        mode: Mutex::new(VideoMode::Synthetic),
        sent: Mutex::new(HashMap::new()),
        arrivals: Mutex::new(arrivals_sender),
        voice: Mutex::new(Vec::new()),
    });
    let (encoder_home, decoder_home) = Home::pair("encoder", "decoder");
    let mut encoder = Daemon::start(&encoder_home, coordinator(&probe));
    let mut decoder = Daemon::start(&decoder_home, coordinator(&probe));
    let (decoder_id, _) = pair_by_qr(&mut encoder, &mut decoder);

    let synthetic = mirror(
        &mut encoder,
        decoder_id,
        &probe,
        &arrivals,
        VideoMode::Synthetic,
    );
    eprintln!("synthetic access units: {}", describe(&synthetic));
    assert!(
        synthetic.protocol.p99 <= P99_TARGET,
        "protocol p99 {:?} is over {P99_TARGET:?}",
        synthetic.protocol.p99
    );

    if vaapi_available() {
        let hardware = mirror(
            &mut encoder,
            decoder_id,
            &probe,
            &arrivals,
            VideoMode::Vaapi,
        );
        eprintln!("VA-API HEVC end to end: {}", describe(&hardware));
        assert!(
            hardware.protocol.p99 <= P99_TARGET,
            "protocol p99 {:?} is over {P99_TARGET:?}",
            hardware.protocol.p99
        );
    } else {
        eprintln!("skipping the VA-API run: no HEVC encode and decode on /dev/dri/renderD128");
    }

    let (frequency, level) = microphone(&mut encoder, decoder_id, &probe);
    eprintln!("tone through Opus and llts audio: {frequency:.1} Hz at RMS {level:.3}");
    assert!(
        (frequency - TONE_HZ).abs() < TONE_HZ * 0.02,
        "the tone came out at {frequency} Hz"
    );
    assert!(level > 0.25, "the tone came out at RMS {level}");

    encoder.stop();
    decoder.stop();
}

/// The encoding daemon's platform in the viewer smoke run: the GPU test pattern for a screen,
/// and no seat access; everything else, the viewer included, is the real one.
#[derive(Debug)]
struct PatternScreen;

impl MediaPlatform for PatternScreen {
    fn screen_source(
        &self,
        _target: CaptureTarget,
        params: &VideoParams,
    ) -> Result<Box<dyn FrameSource>, FeatureError> {
        Ok(Box::new(TestPatternSource::encoded(
            params,
            &VaapiDevice::open_default()?,
        )?))
    }

    fn injector(&self, _output: Option<String>) -> Result<Injector, FeatureError> {
        Err(FeatureError::Unsupported("input injection in this test"))
    }
}

/// Opens a real `crownconnect-viewer` window for three seconds of mirrored test pattern; run
/// on a desktop with `CROWNCONNECT_VIEWER=target/debug/crownconnect-viewer`.
#[test]
#[ignore = "opens a window on the running compositor"]
fn a_mirrored_pattern_plays_in_the_viewer() {
    support::init_logging();
    let (encoder_home, decoder_home) = Home::pair("pattern", "viewer");
    let mut encoder = Daemon::start(
        &encoder_home,
        Box::new(FeatureCoordinator::new(Arc::new(PatternScreen))),
    );
    let mut decoder = Daemon::start(
        &decoder_home,
        Box::new(FeatureCoordinator::new(Arc::new(PatternScreen))),
    );
    let (decoder_id, _) = pair_by_qr(&mut encoder, &mut decoder);
    let options = MirrorOptions {
        direction: Direction::ToPeer,
        limits: VideoLimits {
            max_width: WIDTH,
            max_height: HEIGHT,
            max_fps: FPS,
        },
        remote_input: true,
    };
    start(&mut encoder, Feature::Mirror, |client| {
        client
            .start_mirror(decoder_id, options)
            .expect("start_mirror")
    });
    std::thread::sleep(Duration::from_secs(3));
    while let Some(event) = decoder.client.next_event() {
        assert!(
            !matches!(event, proto::Event::FeatureFailed(_)),
            "the viewer failed: {event:?}"
        );
    }
    let _ = decoder.client.handle_readable();
    while let Some(event) = decoder.client.next_event() {
        assert!(
            !matches!(event, proto::Event::FeatureFailed(_)),
            "the viewer failed: {event:?}"
        );
    }
    let pending = encoder
        .client
        .stop(decoder_id, Feature::Mirror)
        .expect("stop");
    encoder.client.wait(pending, REPLY).expect("mirror stopped");
    encoder.stop();
    decoder.stop();
}
