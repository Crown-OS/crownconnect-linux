//! Voice audio: Opus with in-band FEC, a lock-free jitter-buffered PCM ring between the network
//! and PipeWire threads, and PipeWire nodes for the remote microphone and local capture.

pub mod opus;
pub mod pcm_ring;
pub mod pw_sink;
pub mod pw_source;
mod pw_thread;

pub use opus::{VoiceConfig, VoiceDecoder, VoiceEncoder};
pub use pcm_ring::{pcm_ring, PcmReader, PcmWriter};
pub use pw_sink::AudioCapture;
pub use pw_source::VirtualMicrophone;
