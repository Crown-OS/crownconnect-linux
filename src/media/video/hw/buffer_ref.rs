use std::ptr::NonNull;

use ffmpeg_next::ffi::{av_buffer_ref, av_buffer_unref, AVBufferRef};

use crate::media::MediaError;

/// An owned reference to a refcounted FFmpeg buffer such as a device or frames context.
#[derive(Debug)]
pub(crate) struct BufferRef(NonNull<AVBufferRef>);

// SAFETY: AVBufferRef refcounting is atomic and the contexts it points at are immutable once
// initialised, so references may move between threads.
unsafe impl Send for BufferRef {}

impl BufferRef {
    /// Takes ownership of one reference, failing on null.
    pub(crate) fn from_raw(
        raw: *mut AVBufferRef,
        operation: &'static str,
    ) -> Result<Self, MediaError> {
        NonNull::new(raw).map(Self).ok_or(MediaError::Ffmpeg {
            operation,
            source: ffmpeg_next::Error::Other {
                errno: ffmpeg_next::util::error::ENOMEM,
            },
        })
    }

    pub(crate) fn new_ref(&self) -> Result<Self, MediaError> {
        // SAFETY: self holds a live reference.
        Self::from_raw(unsafe { av_buffer_ref(self.0.as_ptr()) }, "av_buffer_ref")
    }

    /// Transfers ownership of the reference to the caller.
    pub(crate) const fn into_raw(self) -> *mut AVBufferRef {
        let raw = self.0.as_ptr();
        std::mem::forget(self);
        raw
    }

    pub(crate) const fn as_ptr(&self) -> *mut AVBufferRef {
        self.0.as_ptr()
    }

    /// The object the buffer wraps, such as an `AVHWDeviceContext`.
    pub(crate) fn data<T>(&self) -> *mut T {
        // SAFETY: self holds a live reference, so its data pointer is valid.
        unsafe { (*self.0.as_ptr()).data.cast() }
    }
}

impl Drop for BufferRef {
    fn drop(&mut self) {
        let mut raw = self.0.as_ptr();
        // SAFETY: self owns exactly one reference, released once here.
        unsafe { av_buffer_unref(&mut raw) };
    }
}
