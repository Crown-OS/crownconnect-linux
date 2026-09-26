use ffmpeg_next::ffi::{av_buffer_ref, AVHWFramesContext, AVPixelFormat};

use crate::media::video::convert::GpuColorConverter;
use crate::media::video::hw::{new_frames_context_on, BufferRef, FramesSpec};
use crate::media::MediaError;

/// Import target and converter for XRGB dmabufs, created on first use.
#[derive(Debug)]
pub(super) struct RgbInput {
    pub(super) frames: BufferRef,
    pub(super) converter: GpuColorConverter,
}

impl RgbInput {
    pub(super) fn new(
        nv12_frames: &BufferRef,
        width: u32,
        height: u32,
    ) -> Result<Self, MediaError> {
        let device = nv12_frames.data::<AVHWFramesContext>();
        // SAFETY: nv12_frames is an initialised frames context, whose device_ref is live.
        let device = BufferRef::from_raw(
            unsafe { av_buffer_ref((*device).device_ref) },
            "av_buffer_ref(device)",
        )?;
        let frames = new_frames_context_on(
            &device,
            FramesSpec {
                software_format: AVPixelFormat::AV_PIX_FMT_BGR0,
                width,
                height,
                pool_size: 0,
            },
        )?;
        let converter = GpuColorConverter::new(&frames, width, height)?;
        Ok(Self { frames, converter })
    }
}
