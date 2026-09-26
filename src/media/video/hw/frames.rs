use ffmpeg_next::ffi::{
    av_hwframe_ctx_alloc, av_hwframe_ctx_init, AVHWFramesContext, AVPixelFormat,
};

use super::{BufferRef, VaapiDevice};
use crate::media::MediaError;
use crate::util::av::av_result;

/// The surfaces a VA-API frames context hands out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FramesSpec {
    pub(crate) software_format: AVPixelFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pool_size: u32,
}

pub(crate) fn new_frames_context(
    device: &VaapiDevice,
    spec: FramesSpec,
) -> Result<BufferRef, MediaError> {
    new_frames_context_on(device.buffer(), spec)
}

/// As [`new_frames_context`], for a raw VAAPI device reference.
pub(crate) fn new_frames_context_on(
    device: &BufferRef,
    spec: FramesSpec,
) -> Result<BufferRef, MediaError> {
    // SAFETY: the device reference is live; alloc returns a new frames reference or null.
    let raw = unsafe { av_hwframe_ctx_alloc(device.as_ptr()) };
    let frames = BufferRef::from_raw(raw, "av_hwframe_ctx_alloc")?;
    let context = frames.data::<AVHWFramesContext>();
    // SAFETY: context points at the freshly allocated, not yet initialised frames context.
    unsafe {
        (*context).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
        (*context).sw_format = spec.software_format;
        (*context).width = dimension(spec.width)?;
        (*context).height = dimension(spec.height)?;
        (*context).initial_pool_size = dimension(spec.pool_size)?;
    }
    // SAFETY: frames is a configured, uninitialised frames context.
    av_result(
        unsafe { av_hwframe_ctx_init(frames.as_ptr()) },
        "av_hwframe_ctx_init",
    )?;
    Ok(frames)
}

pub(crate) fn dimension(value: u32) -> Result<i32, MediaError> {
    i32::try_from(value).map_err(|_| MediaError::InvalidFrame("dimension exceeds i32"))
}
