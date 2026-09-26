//! VA-API video: capability probing and negotiation, a low-latency encoder fed by zero-copy
//! dmabufs, and a decoder whose surfaces export back to dmabufs for display.

pub mod codec;
mod convert;
pub mod decode;
pub mod dmabuf;
pub mod encode;
pub mod hw;
mod image;
mod map;
pub mod negotiate;
pub mod rate;
#[cfg(test)]
mod ring_tests;
pub mod slot_cache;

pub use codec::{BitDepth, CodecProfile, EncoderBackend, ProfileSet, VideoCodec};
pub use decode::{DecodedFrame, DownloadBuffer, VideoDecoder};
pub use dmabuf::{BufferIdentity, DmabufFrame, DrmFourcc, PlaneLayout};
pub use encode::{EncodedUnit, EncoderConfig, RateControl, VideoEncoder};
pub use hw::{HardwareCapabilities, VaapiDevice};
pub use image::Nv12Image;
pub use map::ExportedDmabuf;
pub use negotiate::{select_profile, CodecCapabilities};
