use std::ffi::{c_void, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use ffmpeg_next::ffi::{
    av_hwdevice_ctx_create, AVHWDeviceContext, AVHWDeviceType, AVVAAPIDeviceContext,
};

use super::BufferRef;
use crate::media::MediaError;
use crate::util::av::av_result;

pub const DEFAULT_RENDER_NODE: &str = "/dev/dri/renderD128";

/// A VA-API device opened on a DRM render node, shared by encoders and decoders.
#[derive(Debug)]
pub struct VaapiDevice {
    device: BufferRef,
}

impl VaapiDevice {
    pub fn open(render_node: &Path) -> Result<Self, MediaError> {
        let path = CString::new(render_node.as_os_str().as_bytes())
            .map_err(|_| MediaError::InvalidFrame("render node path contains a NUL byte"))?;
        let mut raw = ptr::null_mut();
        // SAFETY: raw receives a new device reference on success; path outlives the call.
        let code = unsafe {
            av_hwdevice_ctx_create(
                &mut raw,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                path.as_ptr(),
                ptr::null_mut(),
                0,
            )
        };
        av_result(code, "av_hwdevice_ctx_create(vaapi)")?;
        Ok(Self {
            device: BufferRef::from_raw(raw, "av_hwdevice_ctx_create(vaapi)")?,
        })
    }

    pub fn open_default() -> Result<Self, MediaError> {
        Self::open(Path::new(DEFAULT_RENDER_NODE))
    }

    pub(crate) const fn buffer(&self) -> &BufferRef {
        &self.device
    }

    pub(crate) fn display(&self) -> *mut c_void {
        let context = self.device.data::<AVHWDeviceContext>();
        // SAFETY: a VAAPI device context's hwctx is an initialised AVVAAPIDeviceContext.
        unsafe { (*(*context).hwctx.cast::<AVVAAPIDeviceContext>()).display }
    }
}
