use std::ptr::{self, NonNull};

use ffmpeg_next::ffi::{
    avcodec_alloc_context3, avcodec_find_decoder_by_name, avcodec_find_encoder_by_name,
    avcodec_free_context, avcodec_open2, avcodec_receive_frame, avcodec_receive_packet,
    avcodec_send_frame, avcodec_send_packet, AVCodec, AVCodecContext, AVPacket,
};
use ffmpeg_next::Dictionary;

use super::{AvFrame, AvPacket};
use crate::media::MediaError;
use crate::util::av::{av_result, is_would_block};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodecRole {
    Encoder,
    Decoder,
}

/// An owned `AVCodecContext` for one hardware encoder or decoder.
#[derive(Debug)]
pub(crate) struct CodecContext(NonNull<AVCodecContext>);

// SAFETY: the context is only reached through &mut self; FFmpeg codec contexts may move between
// threads when not used concurrently.
unsafe impl Send for CodecContext {}

impl CodecContext {
    pub(crate) fn new(name: &'static str, role: CodecRole) -> Result<Self, MediaError> {
        let codec_name = std::ffi::CString::new(name)
            .map_err(|_| MediaError::Unsupported("codec name contains a NUL byte"))?;
        // SAFETY: codec_name is a valid C string for the duration of the lookup.
        let codec: *const AVCodec = unsafe {
            match role {
                CodecRole::Encoder => avcodec_find_encoder_by_name(codec_name.as_ptr()),
                CodecRole::Decoder => avcodec_find_decoder_by_name(codec_name.as_ptr()),
            }
        };
        if codec.is_null() {
            return Err(MediaError::Unsupported(name));
        }
        // SAFETY: codec is a registered codec descriptor.
        NonNull::new(unsafe { avcodec_alloc_context3(codec) })
            .map(Self)
            .ok_or(MediaError::Unsupported("avcodec_alloc_context3 failed"))
    }

    pub(crate) const fn get_mut(&mut self) -> &mut AVCodecContext {
        // SAFETY: the pointer is live and uniquely owned by self.
        unsafe { self.0.as_mut() }
    }

    pub(crate) fn open(&mut self, options: Dictionary<'_>) -> Result<(), MediaError> {
        // SAFETY: ownership of the dictionary moves to FFmpeg for the open call.
        let mut raw_options = unsafe { options.disown() };
        // SAFETY: the context is configured and not yet open; FFmpeg consumes recognised
        // options and leaves the rest in raw_options, which is freed below.
        let code = unsafe { avcodec_open2(self.0.as_ptr(), ptr::null(), &mut raw_options) };
        // SAFETY: raw_options is owned here again after avcodec_open2 returns.
        drop(unsafe { Dictionary::own(raw_options) });
        av_result(code, "avcodec_open2").map(drop)
    }

    pub(crate) fn send_frame(&mut self, frame: Option<&AvFrame>) -> Result<(), MediaError> {
        let frame = frame.map_or(ptr::null(), AvFrame::as_ptr);
        // SAFETY: the context is open and frame is null or a valid frame.
        av_result(
            unsafe { avcodec_send_frame(self.0.as_ptr(), frame) },
            "avcodec_send_frame",
        )
        .map(drop)
    }

    pub(crate) fn send_packet(&mut self, packet: *const AVPacket) -> Result<(), MediaError> {
        // SAFETY: the context is open and packet is null or a valid packet.
        av_result(
            unsafe { avcodec_send_packet(self.0.as_ptr(), packet) },
            "avcodec_send_packet",
        )
        .map(drop)
    }

    /// Returns false when the encoder needs more input.
    pub(crate) fn receive_packet(&mut self, packet: &mut AvPacket) -> Result<bool, MediaError> {
        // SAFETY: the context is open and packet is a valid, reusable packet.
        let code = unsafe { avcodec_receive_packet(self.0.as_ptr(), packet.as_mut_ptr()) };
        would_block_as_false(code, "avcodec_receive_packet")
    }

    /// Returns false when the decoder needs more input.
    pub(crate) fn receive_frame(&mut self, frame: &mut AvFrame) -> Result<bool, MediaError> {
        // SAFETY: the context is open and frame is a valid, reusable frame.
        let code = unsafe { avcodec_receive_frame(self.0.as_ptr(), frame.as_mut_ptr()) };
        would_block_as_false(code, "avcodec_receive_frame")
    }
}

fn would_block_as_false(code: i32, operation: &'static str) -> Result<bool, MediaError> {
    match av_result(code, operation) {
        Ok(_) => Ok(true),
        Err(MediaError::Ffmpeg { source, .. })
            if is_would_block(source) || source == ffmpeg_next::Error::Eof =>
        {
            Ok(false)
        }
        Err(other) => Err(other),
    }
}

impl Drop for CodecContext {
    fn drop(&mut self) {
        let mut raw = self.0.as_ptr();
        // SAFETY: self owns the context, freed once here together with its hw references.
        unsafe { avcodec_free_context(&mut raw) };
    }
}
