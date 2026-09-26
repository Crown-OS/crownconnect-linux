use std::ptr;

use ffmpeg_next::ffi::{
    av_hwframe_transfer_data, AVPixelFormat, AV_CODEC_FLAG_LOW_DELAY, FF_THREAD_SLICE,
};

use super::codec::VideoCodec;
use super::hw::{AvFrame, AvPacket, CodecContext, CodecRole, VaapiDevice};
use super::image::Nv12Image;
use super::map::ExportedDmabuf;
use crate::media::MediaError;
use crate::util::av::av_result;

/// Decoded surfaces a consumer may hold (for example while the compositor scans one out) on top
/// of what the decoder itself references.
const HELD_SURFACES: i32 = 4;

/// A low-latency VA-API decoder: output as soon as a frame is complete, no frame threading.
#[derive(Debug)]
pub struct VideoDecoder {
    context: CodecContext,
    packet: AvPacket,
    spare: Option<AvFrame>,
}

impl VideoDecoder {
    pub fn new(device: &VaapiDevice, codec: VideoCodec) -> Result<Self, MediaError> {
        let mut context = CodecContext::new(decoder_name(codec), CodecRole::Decoder)?;
        let raw = context.get_mut();
        raw.hw_device_ctx = device.buffer().new_ref()?.into_raw();
        raw.flags |= AV_CODEC_FLAG_LOW_DELAY as i32;
        raw.thread_count = 1;
        raw.thread_type = FF_THREAD_SLICE;
        raw.extra_hw_frames = HELD_SURFACES;
        context.open(ffmpeg_next::Dictionary::new())?;
        Ok(Self {
            context,
            packet: AvPacket::new()?,
            spare: None,
        })
    }

    /// Queues one access unit; FFmpeg copies it, so `unit` may be reused right away.
    pub fn decode(&mut self, unit: &[u8], pts: i64) -> Result<(), MediaError> {
        let size = i32::try_from(unit.len())
            .map_err(|_| MediaError::InvalidFrame("access unit exceeds i32::MAX bytes"))?;
        let packet = self.packet.as_mut_ptr();
        // SAFETY: the packet borrows unit only for the duration of send_packet, which copies
        // non-refcounted data, and is reset before returning.
        unsafe {
            (*packet).data = unit.as_ptr().cast_mut();
            (*packet).size = size;
            (*packet).pts = pts;
        }
        let sent = self.context.send_packet(packet);
        // SAFETY: as above; the packet never owned the borrowed bytes.
        unsafe {
            (*packet).data = ptr::null_mut();
            (*packet).size = 0;
        }
        sent
    }

    pub fn next_frame(&mut self) -> Result<Option<DecodedFrame>, MediaError> {
        let mut frame = match self.spare.take() {
            Some(frame) => frame,
            None => AvFrame::new()?,
        };
        if !self.context.receive_frame(&mut frame)? {
            self.spare = Some(frame);
            return Ok(None);
        }
        if frame.get().format != AVPixelFormat::AV_PIX_FMT_VAAPI as i32 {
            return Err(MediaError::Unsupported(
                "decoder fell back to software decoding",
            ));
        }
        Ok(Some(DecodedFrame { surface: frame }))
    }
}

const fn decoder_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::Hevc => "hevc",
        VideoCodec::Av1 => "av1",
    }
}

/// A decoded frame still on the GPU. Holding it keeps its surface out of the decoder's pool.
#[derive(Debug)]
pub struct DecodedFrame {
    surface: AvFrame,
}

impl DecodedFrame {
    pub const fn pts(&self) -> i64 {
        self.surface.get().pts
    }

    pub fn width(&self) -> u32 {
        u32::try_from(self.surface.get().width).unwrap_or_default()
    }

    pub fn height(&self) -> u32 {
        u32::try_from(self.surface.get().height).unwrap_or_default()
    }

    /// The VA surface id, stable while the decoder recycles its pool.
    pub fn surface_id(&self) -> u32 {
        u32::try_from(self.surface.get().data[3] as usize).unwrap_or(u32::MAX)
    }

    /// Exports the surface as a dmabuf for zero-copy display or re-encoding.
    pub fn export_dmabuf(&self) -> Result<ExportedDmabuf, MediaError> {
        ExportedDmabuf::export(&self.surface)
    }

    /// Copies the frame to system memory, reusing `target`'s buffers when the size matches.
    pub fn download<'a>(
        &self,
        target: &'a mut DownloadBuffer,
    ) -> Result<Nv12Image<'a>, MediaError> {
        let frame = &mut target.frame;
        if (frame.get().width, frame.get().height)
            != (self.surface.get().width, self.surface.get().height)
        {
            frame.unref();
        }
        frame.get_mut().format = AVPixelFormat::AV_PIX_FMT_NV12 as i32;
        // SAFETY: frame is empty or a matching NV12 frame; surface is a live VAAPI frame.
        let code =
            unsafe { av_hwframe_transfer_data(frame.as_mut_ptr(), self.surface.as_ptr(), 0) };
        av_result(code, "av_hwframe_transfer_data(download)")?;
        target.image()
    }
}

/// A reusable system-memory NV12 frame for [`DecodedFrame::download`].
#[derive(Debug)]
pub struct DownloadBuffer {
    frame: AvFrame,
}

impl DownloadBuffer {
    pub fn new() -> Result<Self, MediaError> {
        Ok(Self {
            frame: AvFrame::new()?,
        })
    }

    fn image(&self) -> Result<Nv12Image<'_>, MediaError> {
        let raw = self.frame.get();
        let width = u32::try_from(raw.width).unwrap_or_default();
        let height = u32::try_from(raw.height).unwrap_or_default();
        let plane =
            |data: *mut u8, linesize: i32, rows: u32| -> Result<(&[u8], usize), MediaError> {
                let stride = usize::try_from(linesize)
                    .map_err(|_| MediaError::InvalidFrame("negative stride"))?;
                if data.is_null() {
                    return Err(MediaError::InvalidFrame("download produced no plane"));
                }
                // SAFETY: FFmpeg allocated `stride * rows` bytes for this plane.
                Ok((
                    unsafe { std::slice::from_raw_parts(data, stride * rows as usize) },
                    stride,
                ))
            };
        let (luma, luma_stride) = plane(raw.data[0], raw.linesize[0], height)?;
        let (chroma, chroma_stride) = plane(raw.data[1], raw.linesize[1], height / 2)?;
        Ok(Nv12Image {
            width,
            height,
            luma,
            luma_stride,
            chroma,
            chroma_stride,
        })
    }
}
