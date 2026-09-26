use std::ffi::c_int;

use ffmpeg_next::util::error::EAGAIN;

use crate::media::MediaError;

pub(crate) fn av_result(code: c_int, operation: &'static str) -> Result<c_int, MediaError> {
    if code < 0 {
        Err(MediaError::Ffmpeg {
            operation,
            source: ffmpeg_next::Error::from(code),
        })
    } else {
        Ok(code)
    }
}

pub(crate) const fn is_would_block(error: ffmpeg_next::Error) -> bool {
    matches!(error, ffmpeg_next::Error::Other { errno: EAGAIN })
}
