//! Microphones across devices: a peer's microphone as a PipeWire source on this computer, and
//! this computer's microphone streamed to a peer, both as 10 ms Opus frames with in-band FEC.

use std::time::{Duration, Instant};

use super::pipeline::{Control, PipelineContext, Stopped};
use super::sink::{VoiceSink, VoiceSource};
use super::FeatureError;
use crate::media::audio::opus::{
    VoiceChannels, FRAME_DURATION, FRAME_SAMPLES, MAX_PACKET_BYTES, SAMPLE_RATE,
};
use crate::media::audio::{
    pcm_ring, AudioCapture, PcmReader, PcmWriter, VirtualMicrophone, VoiceConfig, VoiceDecoder,
    VoiceEncoder,
};
use crate::peer::EncodedFrame;

/// A one-second ring, primed with 20 ms and never more than 60 ms behind.
const RING_SAMPLES: usize = 48_000;
const JITTER_PREFILL: usize = FRAME_SAMPLES * 2;
const MAX_BUFFERED: usize = FRAME_SAMPLES * 6;
const PACKET_WAIT: Duration = Duration::from_millis(50);
const FRAMES_PER_SECOND: u32 = 100;
const FRAME_TICKS: u32 = SAMPLE_RATE / FRAMES_PER_SECOND;

/// Plays a peer's microphone into a virtual PipeWire source until stopped. A single lost packet
/// is rebuilt from the FEC in the one after it.
pub(crate) fn run_decoder(
    context: &PipelineContext,
    sink: &mut dyn VoiceSink,
) -> Result<(), FeatureError> {
    let mut decoder = VoiceDecoder::new(VoiceChannels::Mono)?;
    let mut pcm = vec![0.0; FRAME_SAMPLES];
    let mut expected: Option<u32> = None;
    loop {
        let frame = match context.next_control(PACKET_WAIT) {
            Ok(Some(Control::Frame(frame))) => frame,
            Ok(_) => continue,
            Err(Stopped) => return Ok(()),
        };
        let timestamp = frame.timestamp().ticks();
        if expected.is_some_and(|expected| timestamp.wrapping_sub(expected) == FRAME_TICKS) {
            let recovered = decoder.recover(Some(frame.bytes()), &mut pcm)?;
            sink.play(pcm.get(..recovered).unwrap_or_default())?;
        }
        let decoded = decoder.decode(frame.bytes(), &mut pcm)?;
        sink.play(pcm.get(..decoded).unwrap_or_default())?;
        expected = Some(timestamp.wrapping_add(FRAME_TICKS));
    }
}

/// Streams this computer's microphone, one Opus packet every 10 ms, until stopped.
pub(crate) fn run_encoder(
    context: &PipelineContext,
    source: &mut dyn VoiceSource,
) -> Result<(), FeatureError> {
    let mut encoder = VoiceEncoder::new(VoiceConfig::default())?;
    let mut pcm = vec![0.0; FRAME_SAMPLES];
    let mut packet = vec![0; MAX_PACKET_BYTES];
    let mut timestamp = 0_u32;
    loop {
        if matches!(context.pending_control(), Err(Stopped)) {
            return Ok(());
        }
        if !source.read_frame(&mut pcm, PACKET_WAIT)? {
            continue;
        }
        let len = encoder.encode(&pcm, &mut packet)?;
        context.send_frame(EncodedFrame {
            bytes: packet.get(..len).unwrap_or_default().to_vec(),
            timestamp,
            keyframe: false,
        });
        timestamp = timestamp.wrapping_add(FRAME_TICKS);
    }
}

/// A PipeWire `Audio/Source` other apps record from.
#[derive(Debug)]
pub(crate) struct VirtualVoice {
    writer: PcmWriter,
    _microphone: VirtualMicrophone,
}

impl VirtualVoice {
    pub(crate) fn open(peer_name: &str) -> Result<Self, FeatureError> {
        let (writer, reader) = pcm_ring(RING_SAMPLES, JITTER_PREFILL, MAX_BUFFERED);
        let microphone = VirtualMicrophone::spawn(peer_name, reader, VoiceChannels::Mono)?;
        Ok(Self {
            writer,
            _microphone: microphone,
        })
    }
}

impl VoiceSink for VirtualVoice {
    fn play(&mut self, pcm: &[f32]) -> Result<(), FeatureError> {
        self.writer.push(pcm);
        Ok(())
    }
}

/// The default PipeWire capture device, read on a steady 10 ms clock.
#[derive(Debug)]
pub(crate) struct CapturedVoice {
    reader: PcmReader,
    next_due: Instant,
    _capture: AudioCapture,
}

impl CapturedVoice {
    pub(crate) fn open() -> Result<Self, FeatureError> {
        let (writer, reader) = pcm_ring(RING_SAMPLES, FRAME_SAMPLES, MAX_BUFFERED);
        let capture = AudioCapture::spawn(None, writer, VoiceChannels::Mono)?;
        Ok(Self {
            reader,
            next_due: Instant::now(),
            _capture: capture,
        })
    }
}

impl VoiceSource for CapturedVoice {
    fn read_frame(&mut self, frame: &mut [f32], timeout: Duration) -> Result<bool, FeatureError> {
        let wait = self.next_due.saturating_duration_since(Instant::now());
        if wait > timeout {
            std::thread::sleep(timeout);
            return Ok(false);
        }
        std::thread::sleep(wait);
        self.next_due += FRAME_DURATION;
        self.reader.fill(frame);
        Ok(true)
    }
}
