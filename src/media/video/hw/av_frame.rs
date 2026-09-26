use std::ptr::NonNull;

use ffmpeg_next::ffi::{
    av_frame_alloc, av_frame_free, av_frame_unref, av_packet_alloc, av_packet_free, AVFrame,
    AVPacket,
};

use crate::media::MediaError;

const fn allocation_failed(operation: &'static str) -> MediaError {
    MediaError::Ffmpeg {
        operation,
        source: ffmpeg_next::Error::Other {
            errno: ffmpeg_next::util::error::ENOMEM,
        },
    }
}

/// An owned `AVFrame`, reused across frames to avoid per-frame allocation.
#[derive(Debug)]
pub(crate) struct AvFrame(NonNull<AVFrame>);

// SAFETY: an AVFrame is only reached through &mut self, and the buffers it references are
// atomically refcounted.
unsafe impl Send for AvFrame {}

impl AvFrame {
    pub(crate) fn new() -> Result<Self, MediaError> {
        // SAFETY: av_frame_alloc has no preconditions.
        NonNull::new(unsafe { av_frame_alloc() })
            .map(Self)
            .ok_or_else(|| allocation_failed("av_frame_alloc"))
    }

    pub(crate) const fn as_ptr(&self) -> *const AVFrame {
        self.0.as_ptr()
    }

    pub(crate) const fn as_mut_ptr(&mut self) -> *mut AVFrame {
        self.0.as_ptr()
    }

    pub(crate) const fn get(&self) -> &AVFrame {
        // SAFETY: the pointer is live and uniquely owned by self.
        unsafe { self.0.as_ref() }
    }

    pub(crate) const fn get_mut(&mut self) -> &mut AVFrame {
        // SAFETY: the pointer is live and uniquely owned by self.
        unsafe { self.0.as_mut() }
    }

    pub(crate) fn unref(&mut self) {
        // SAFETY: the frame is live; unref resets it to the freshly allocated state.
        unsafe { av_frame_unref(self.0.as_ptr()) };
    }
}

impl Drop for AvFrame {
    fn drop(&mut self) {
        let mut raw = self.0.as_ptr();
        // SAFETY: self owns the frame, freed once here.
        unsafe { av_frame_free(&mut raw) };
    }
}

/// An owned `AVPacket`, reused for every access unit.
#[derive(Debug)]
pub(crate) struct AvPacket(NonNull<AVPacket>);

// SAFETY: as for AvFrame.
unsafe impl Send for AvPacket {}

impl AvPacket {
    pub(crate) fn new() -> Result<Self, MediaError> {
        // SAFETY: av_packet_alloc has no preconditions.
        NonNull::new(unsafe { av_packet_alloc() })
            .map(Self)
            .ok_or_else(|| allocation_failed("av_packet_alloc"))
    }

    pub(crate) const fn as_mut_ptr(&mut self) -> *mut AVPacket {
        self.0.as_ptr()
    }

    pub(crate) const fn get(&self) -> &AVPacket {
        // SAFETY: the pointer is live and uniquely owned by self.
        unsafe { self.0.as_ref() }
    }

    pub(crate) fn data(&self) -> &[u8] {
        let packet = self.get();
        match (packet.data.is_null(), usize::try_from(packet.size)) {
            // SAFETY: a non-null packet holds `size` readable bytes until it is unreffed.
            (false, Ok(len)) => unsafe { std::slice::from_raw_parts(packet.data, len) },
            _ => &[],
        }
    }
}

impl Drop for AvPacket {
    fn drop(&mut self) {
        let mut raw = self.0.as_ptr();
        // SAFETY: self owns the packet, freed once here.
        unsafe { av_packet_free(&mut raw) };
    }
}
