use pipewire as pw;
use pw::properties::properties;
use pw::spa::pod::Pod;
use pw::spa::utils::Direction;
use pw::stream::{StreamFlags, StreamRc};

use super::opus::VoiceChannels;
use super::pcm_ring::PcmWriter;
use super::pw_thread::{sanitized, voice_format_pod, PipeWireThread};
use crate::media::MediaError;

const SAMPLE_BYTES: usize = size_of::<f32>();
const SCRATCH_SAMPLES: usize = 4096;
const TEN_MS_QUANTUM: &str = "480/48000";

/// Captures audio from a PipeWire node (the default source when no target is given) into a
/// [`PcmWriter`], from which the network thread takes 10 ms frames to encode and send.
#[derive(Debug)]
pub struct AudioCapture {
    _thread: PipeWireThread,
}

struct CaptureState {
    writer: PcmWriter,
    scratch: Vec<f32>,
}

impl AudioCapture {
    pub fn spawn(
        target: Option<&str>,
        writer: PcmWriter,
        channels: VoiceChannels,
    ) -> Result<Self, MediaError> {
        let target = target.map(sanitized);
        let format = voice_format_pod(channels)?;
        let thread = PipeWireThread::spawn("crownconnect-capture", move |core| {
            let mut props = properties! {
                *pw::keys::MEDIA_TYPE => "Audio",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Communication",
                *pw::keys::NODE_NAME => "crownconnect_capture",
                *pw::keys::NODE_LATENCY => TEN_MS_QUANTUM,
            };
            if let Some(target) = target {
                props.insert(*pw::keys::TARGET_OBJECT, target);
            }
            let stream = StreamRc::new(core.clone(), "crownconnect-capture", props)?;
            let state = CaptureState {
                writer,
                scratch: vec![0.0; SCRATCH_SAMPLES],
            };
            let listener = stream
                .add_local_listener_with_user_data(state)
                .process(capture_buffered)
                .register()?;
            let format = Pod::from_bytes(&format)
                .ok_or(MediaError::Unsupported("invalid audio format pod"))?;
            stream.connect(
                Direction::Input,
                None,
                StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
                &mut [format],
            )?;
            Ok((stream, listener))
        })?;
        Ok(Self { _thread: thread })
    }
}

fn capture_buffered(stream: &pw::stream::Stream, state: &mut CaptureState) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let offset = data.chunk().offset() as usize;
    let size = data.chunk().size() as usize;
    let Some(bytes) = data
        .data()
        .and_then(|bytes| bytes.get(offset..offset + size))
    else {
        return;
    };
    let (samples, _) = bytes.as_chunks::<SAMPLE_BYTES>();
    for block in samples.chunks(state.scratch.len()) {
        let scratch = state.scratch.get_mut(..block.len()).unwrap_or_default();
        scratch
            .iter_mut()
            .zip(block)
            .for_each(|(sample, raw)| *sample = f32::from_le_bytes(*raw));
        state.writer.push(scratch);
    }
}
