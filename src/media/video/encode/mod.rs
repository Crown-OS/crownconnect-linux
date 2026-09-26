mod context;
mod rgb_input;
mod submit;

use ffmpeg_next::ffi::{av_hwframe_get_buffer, av_hwframe_transfer_data, AV_PKT_FLAG_KEY};

use super::codec::{CodecProfile, EncoderBackend};
use super::dmabuf::{BufferIdentity, DmabufFrame, DrmFourcc};
use super::hw::{
    new_frames_context, AvFrame, AvPacket, BufferRef, CodecContext, EncodeFeatures, FramesSpec,
    HardwareCapabilities, VaapiDevice,
};
use super::image::Nv12Image;
use super::map::map_dmabuf;
use super::rate::{plan_rate_change, RateAdjustment};
use super::slot_cache::SlotCache;
use crate::media::MediaError;
use crate::util::av::av_result;
use context::{open_context, qp_range, software_format};
use rgb_input::RgbInput;
use submit::{describe_nv12, submit, Submission};

const UPLOAD_POOL_SIZE: u32 = 0;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    #[default]
    Cbr,
    Vbr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub profile: CodecProfile,
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub bitrate_bps: u32,
    pub rate_control: RateControl,
    /// Slices per frame, so the network layer can packetise and conceal loss per slice.
    pub slices: u32,
    pub backend: EncoderBackend,
}

/// One encoded access unit, borrowed from the encoder until the next call.
#[derive(Debug, Clone, Copy)]
pub struct EncodedUnit<'a> {
    /// Annex B bytes (OBUs for AV1); split with [`crate::util::annexb::nal_units`].
    pub data: &'a [u8],
    pub keyframe: bool,
    /// Capture time in microseconds, as passed to the encode call.
    pub pts: i64,
}

/// A low-latency VA-API encoder: no B-frames, one frame in flight, an unbounded GOP with IDR on
/// request, and a one-frame rate control buffer so each frame stays within its byte budget.
#[derive(Debug)]
pub struct VideoEncoder {
    config: EncoderConfig,
    features: EncodeFeatures,
    frames: BufferRef,
    context: CodecContext,
    packet: AvPacket,
    upload: AvFrame,
    upload_source: AvFrame,
    mapped: SlotCache<BufferIdentity, AvFrame>,
    rgb_input: Option<RgbInput>,
    dmabuf_imports: u64,
    configured_bps: u32,
    qp_offset: i8,
    keyframe_requested: bool,
    reconfigure_pending: bool,
}

impl VideoEncoder {
    pub fn new(
        device: &VaapiDevice,
        capabilities: &HardwareCapabilities,
        config: EncoderConfig,
    ) -> Result<Self, MediaError> {
        if !capabilities.codecs.encode.contains(config.profile) {
            return Err(MediaError::Unsupported(
                "profile has no hardware encoder here",
            ));
        }
        let features = capabilities.encode_features(config.profile);
        let frames = new_frames_context(
            device,
            FramesSpec {
                software_format: software_format(config.profile),
                width: config.width,
                height: config.height,
                pool_size: UPLOAD_POOL_SIZE,
            },
        )?;
        let context = open_context(&config, features, &frames, config.bitrate_bps)?;
        Ok(Self {
            config,
            features,
            frames,
            context,
            packet: AvPacket::new()?,
            upload: AvFrame::new()?,
            upload_source: AvFrame::new()?,
            mapped: SlotCache::default(),
            rgb_input: None,
            dmabuf_imports: 0,
            configured_bps: config.bitrate_bps,
            qp_offset: 0,
            keyframe_requested: false,
            reconfigure_pending: false,
        })
    }

    pub const fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// Follows the congestion controller: small changes shift per-frame QP immediately, large
    /// ones rebuild rate control on the next frame.
    pub fn set_target_bitrate(&mut self, bits_per_second: u32) {
        self.config.bitrate_bps = bits_per_second;
        match plan_rate_change(
            self.configured_bps,
            bits_per_second,
            self.features.roi_qp_delta,
        ) {
            RateAdjustment::Keep => self.qp_offset = 0,
            RateAdjustment::QpOffset(offset) => self.qp_offset = offset,
            RateAdjustment::Reconfigure => self.reconfigure_pending = true,
        }
    }

    /// Makes the next frame an IDR, as a peer asks after unrecoverable loss.
    pub const fn request_keyframe(&mut self) {
        self.keyframe_requested = true;
    }

    /// Encodes a captured dmabuf in place. Each ring slot's buffer is imported as a VA surface
    /// once and reused while the slot keeps the same buffer. XRGB input is converted to NV12 by
    /// the GPU's video processor on the way.
    pub fn encode_dmabuf(
        &mut self,
        slot: usize,
        frame: &DmabufFrame<'_>,
        pts: i64,
    ) -> Result<(), MediaError> {
        self.check_size(frame.width, frame.height)?;
        self.apply_pending_reconfigure()?;
        if frame.fourcc == DrmFourcc::Xrgb8888 && self.rgb_input.is_none() {
            self.rgb_input = Some(RgbInput::new(
                &self.frames,
                self.config.width,
                self.config.height,
            )?);
        }
        let identity = frame.identity()?;
        let submission = self.submission(pts);
        let frames = match (&self.rgb_input, frame.fourcc) {
            (Some(rgb), DrmFourcc::Xrgb8888) => &rgb.frames,
            _ => &self.frames,
        };
        let imports = &mut self.dmabuf_imports;
        let surface = self.mapped.get_or_map(slot, identity, || {
            *imports += 1;
            map_dmabuf(frames, frame)
        })?;
        match (&mut self.rgb_input, frame.fourcc) {
            (Some(rgb), DrmFourcc::Xrgb8888) => submit(
                &mut self.context,
                rgb.converter.convert(surface)?,
                submission,
            ),
            _ => submit(&mut self.context, surface, submission),
        }
    }

    /// How many times a dmabuf was imported as a VA surface; stays at the ring depth while the
    /// capture ring is stable.
    pub const fn dmabuf_imports(&self) -> u64 {
        self.dmabuf_imports
    }

    /// Uploads a system-memory frame and encodes it; for tests and non-dmabuf sources.
    pub fn encode_nv12(&mut self, image: &Nv12Image<'_>, pts: i64) -> Result<(), MediaError> {
        image.validate()?;
        self.check_size(image.width, image.height)?;
        self.apply_pending_reconfigure()?;
        describe_nv12(&mut self.upload_source, image);
        self.upload.unref();
        // SAFETY: frames is an initialised VAAPI frames context; upload is an empty frame.
        let code =
            unsafe { av_hwframe_get_buffer(self.frames.as_ptr(), self.upload.as_mut_ptr(), 0) };
        av_result(code, "av_hwframe_get_buffer")?;
        // SAFETY: source describes caller memory that outlives the call; upload is a surface.
        let code = unsafe {
            av_hwframe_transfer_data(self.upload.as_mut_ptr(), self.upload_source.as_ptr(), 0)
        };
        self.upload_source.unref();
        av_result(code, "av_hwframe_transfer_data(upload)")?;
        let submission = self.submission(pts);
        submit(&mut self.context, &mut self.upload, submission)
    }

    /// The next finished access unit, if the hardware has one.
    pub fn next_unit(&mut self) -> Result<Option<EncodedUnit<'_>>, MediaError> {
        if !self.context.receive_packet(&mut self.packet)? {
            return Ok(None);
        }
        let packet = self.packet.get();
        Ok(Some(EncodedUnit {
            keyframe: packet.flags & AV_PKT_FLAG_KEY != 0,
            pts: packet.pts,
            data: self.packet.data(),
        }))
    }

    /// Drops every cached dmabuf import, as when the capture ring is reallocated.
    pub fn forget_ring(&mut self) {
        self.mapped.clear();
    }

    fn submission(&mut self, pts: i64) -> Submission {
        Submission {
            pts,
            keyframe: std::mem::take(&mut self.keyframe_requested),
            qp_offset: self.qp_offset,
            qp_range: qp_range(self.config.profile.codec()),
            width: self.config.width,
            height: self.config.height,
        }
    }

    fn check_size(&self, width: u32, height: u32) -> Result<(), MediaError> {
        if (width, height) == (self.config.width, self.config.height) {
            Ok(())
        } else {
            Err(MediaError::InvalidFrame(
                "frame size differs from the encoder's",
            ))
        }
    }

    fn apply_pending_reconfigure(&mut self) -> Result<(), MediaError> {
        if !std::mem::take(&mut self.reconfigure_pending) {
            return Ok(());
        }
        let bitrate = self.config.bitrate_bps;
        self.context = open_context(&self.config, self.features, &self.frames, bitrate)?;
        self.configured_bps = bitrate;
        self.qp_offset = 0;
        self.keyframe_requested = true;
        Ok(())
    }
}
