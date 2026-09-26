use pipewire as pw;
use pw::properties::properties;
use pw::spa::pod::Pod;
use pw::spa::utils::Direction;
use pw::stream::{StreamFlags, StreamRc};

use super::opus::VoiceChannels;
use super::pcm_ring::PcmReader;
use super::pw_thread::{sanitized, voice_format_pod, PipeWireThread};
use crate::media::MediaError;

const SAMPLE_BYTES: usize = size_of::<f32>();
const SCRATCH_SAMPLES: usize = 4096;
const TEN_MS_QUANTUM: &str = "480/48000";

/// A virtual `Audio/Source` node, "CrownConnect Microphone (<device>)", that apps record from.
/// It plays whatever the network thread decodes into the paired [`PcmReader`].
#[derive(Debug)]
pub struct VirtualMicrophone {
    node_name: String,
    _thread: PipeWireThread,
}

struct MicrophoneState {
    reader: PcmReader,
    scratch: Vec<f32>,
    frame_bytes: usize,
}

impl VirtualMicrophone {
    pub fn spawn(
        device_name: &str,
        reader: PcmReader,
        channels: VoiceChannels,
    ) -> Result<Self, MediaError> {
        let device_name = sanitized(device_name);
        let description = format!("CrownConnect Microphone ({device_name})");
        let node_name = node_name(&device_name);
        let format = voice_format_pod(channels)?;
        let stream_node_name = node_name.clone();
        let thread = PipeWireThread::spawn("crownconnect-mic", move |core| {
            let stream = StreamRc::new(
                core.clone(),
                "crownconnect-mic",
                properties! {
                    *pw::keys::MEDIA_TYPE => "Audio",
                    *pw::keys::MEDIA_CLASS => "Audio/Source",
                    *pw::keys::MEDIA_ROLE => "Communication",
                    *pw::keys::NODE_NAME => stream_node_name,
                    *pw::keys::NODE_DESCRIPTION => description,
                    *pw::keys::NODE_LATENCY => TEN_MS_QUANTUM,
                },
            )?;
            let state = MicrophoneState {
                reader,
                scratch: vec![0.0; SCRATCH_SAMPLES],
                frame_bytes: SAMPLE_BYTES * channels.count(),
            };
            let listener = stream
                .add_local_listener_with_user_data(state)
                .process(play_buffered)
                .register()?;
            let format = Pod::from_bytes(&format)
                .ok_or(MediaError::Unsupported("invalid audio format pod"))?;
            stream.connect(
                Direction::Output,
                None,
                StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
                &mut [format],
            )?;
            Ok((stream, listener))
        })?;
        Ok(Self {
            node_name,
            _thread: thread,
        })
    }

    /// The PipeWire `node.name`, for finding or linking the node.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }
}

fn node_name(device_name: &str) -> String {
    let slug: String = device_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("crownconnect_mic_{slug}")
}

fn play_buffered(stream: &pw::stream::Stream, state: &mut MicrophoneState) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let requested = usize::try_from(buffer.requested()).unwrap_or_default();
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let frame_bytes = state.frame_bytes;
    let written = data.data().map_or(0, |bytes| {
        let capacity = bytes.len() / frame_bytes;
        let frames = if requested == 0 {
            capacity
        } else {
            requested.min(capacity)
        };
        let (samples, _) = bytes.as_chunks_mut::<SAMPLE_BYTES>();
        let samples = samples
            .get_mut(..frames * frame_bytes / SAMPLE_BYTES)
            .unwrap_or_default();
        for block in samples.chunks_mut(state.scratch.len()) {
            let scratch = state.scratch.get_mut(..block.len()).unwrap_or_default();
            state.reader.fill(scratch);
            block
                .iter_mut()
                .zip(scratch.iter())
                .for_each(|(out, sample)| *out = sample.to_le_bytes());
        }
        frames * frame_bytes
    });
    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = i32::try_from(frame_bytes).unwrap_or_default();
    *chunk.size_mut() = u32::try_from(written).unwrap_or_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_names_are_stable_ascii_slugs() {
        assert_eq!(node_name("Pixel 9 Pro"), "crownconnect_mic_pixel_9_pro");
    }
}
