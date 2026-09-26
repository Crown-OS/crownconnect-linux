//! Feeds decoded peer camera frames to apps through a v4l2loopback device.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;

use v4l::video::Output;
use v4l::{Device, Format, FourCC};

use crate::media::video::Nv12Image;
use crate::media::MediaError;
use crate::util::yuv::{nv12_to_yuyv, pack_nv12};

/// The pixel layout a sink hands to its consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkPixelFormat {
    Nv12,
    /// Packed 4:2:2, which the widest range of camera apps accept.
    Yuyv,
}

impl SinkPixelFormat {
    const fn fourcc(self) -> &'static [u8; 4] {
        match self {
            Self::Nv12 => b"NV12",
            Self::Yuyv => b"YUYV",
        }
    }

    fn pack(self, image: &Nv12Image<'_>, out: &mut Vec<u8>) {
        match self {
            Self::Nv12 => pack_nv12(image, out),
            Self::Yuyv => nv12_to_yuyv(image, out),
        }
    }
}

/// Somewhere decoded frames go to be consumed as a camera.
pub trait FrameSink {
    fn write_frame(&mut self, image: &Nv12Image<'_>) -> Result<(), MediaError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SinkGeometry {
    width: u32,
    height: u32,
    format: SinkPixelFormat,
}

impl SinkGeometry {
    fn pack(&self, image: &Nv12Image<'_>, out: &mut Vec<u8>) -> Result<(), MediaError> {
        image.validate()?;
        if (image.width, image.height) != (self.width, self.height) {
            return Err(MediaError::InvalidFrame(
                "frame size differs from the sink's",
            ));
        }
        self.format.pack(image, out);
        Ok(())
    }
}

/// A v4l2loopback output device. Frames are packed into one reused staging buffer and written
/// with a single `write`, which v4l2loopback treats as one frame.
pub struct V4l2LoopbackSink {
    device: Device,
    geometry: SinkGeometry,
    staging: Vec<u8>,
}

impl V4l2LoopbackSink {
    pub fn open(
        path: &Path,
        width: u32,
        height: u32,
        format: SinkPixelFormat,
    ) -> Result<Self, MediaError> {
        let device = Device::with_path(path)?;
        let requested = Format::new(width, height, FourCC::new(format.fourcc()));
        let applied = Output::set_format(&device, &requested)?;
        if applied.fourcc != requested.fourcc || (applied.width, applied.height) != (width, height)
        {
            return Err(MediaError::Unsupported(
                "v4l2 device rejected the frame format",
            ));
        }
        Ok(Self {
            device,
            geometry: SinkGeometry {
                width,
                height,
                format,
            },
            staging: Vec::new(),
        })
    }
}

impl std::fmt::Debug for V4l2LoopbackSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("V4l2LoopbackSink")
            .field("fd", &self.device.handle().fd())
            .field("geometry", &self.geometry)
            .finish_non_exhaustive()
    }
}

impl FrameSink for V4l2LoopbackSink {
    fn write_frame(&mut self, image: &Nv12Image<'_>) -> Result<(), MediaError> {
        self.geometry.pack(image, &mut self.staging)?;
        self.device.write_all(&self.staging)?;
        Ok(())
    }
}

/// An in-memory sink that keeps the most recent frames, for tests and previews.
#[derive(Debug)]
pub struct MemorySink {
    geometry: SinkGeometry,
    frames: VecDeque<Vec<u8>>,
    capacity: usize,
    written: u64,
}

impl MemorySink {
    pub fn new(width: u32, height: u32, format: SinkPixelFormat, capacity: usize) -> Self {
        Self {
            geometry: SinkGeometry {
                width,
                height,
                format,
            },
            frames: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
            written: 0,
        }
    }

    pub fn latest(&self) -> Option<&[u8]> {
        self.frames.back().map(Vec::as_slice)
    }

    pub const fn written(&self) -> u64 {
        self.written
    }
}

impl FrameSink for MemorySink {
    fn write_frame(&mut self, image: &Nv12Image<'_>) -> Result<(), MediaError> {
        let mut frame = if self.frames.len() == self.capacity {
            self.frames.pop_front().unwrap_or_default()
        } else {
            Vec::new()
        };
        self.geometry.pack(image, &mut frame)?;
        self.frames.push_back(frame);
        self.written += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LUMA: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    const CHROMA: [u8; 4] = [10, 20, 30, 40];

    fn image() -> Nv12Image<'static> {
        Nv12Image {
            width: 4,
            height: 2,
            luma: &LUMA,
            luma_stride: 4,
            chroma: &CHROMA,
            chroma_stride: 4,
        }
    }

    #[test]
    fn memory_sink_keeps_the_latest_frames_in_the_requested_layout() -> Result<(), MediaError> {
        let mut sink = MemorySink::new(4, 2, SinkPixelFormat::Yuyv, 2);
        for _ in 0..3 {
            sink.write_frame(&image())?;
        }
        assert_eq!(sink.written(), 3);
        assert_eq!(sink.frames.len(), 2);
        assert_eq!(
            sink.latest(),
            Some(&[1, 10, 2, 20, 3, 30, 4, 40, 5, 10, 6, 20, 7, 30, 8, 40][..])
        );
        Ok(())
    }

    #[test]
    fn nv12_sink_packs_planes_back_to_back() -> Result<(), MediaError> {
        let mut sink = MemorySink::new(4, 2, SinkPixelFormat::Nv12, 1);
        sink.write_frame(&image())?;
        assert_eq!(sink.latest().map(<[u8]>::len), Some(12));
        Ok(())
    }

    #[test]
    fn rejects_frames_of_another_size() {
        let mut sink = MemorySink::new(8, 2, SinkPixelFormat::Nv12, 1);
        assert!(sink.write_frame(&image()).is_err());
    }

    #[test]
    fn opening_a_missing_device_fails_cleanly() {
        let missing = Path::new("/dev/crownconnect-no-such-video-device");
        assert!(V4l2LoopbackSink::open(missing, 640, 480, SinkPixelFormat::Yuyv).is_err());
    }
}
