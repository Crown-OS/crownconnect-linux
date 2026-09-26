use std::mem::size_of;
use std::os::fd::BorrowedFd;

use ffmpeg_next::ffi::{
    av_buffer_allocz, av_hwframe_map, AVDRMFrameDescriptor, AVPixelFormat, AV_HWFRAME_MAP_READ,
};

use super::dmabuf::{DmabufFrame, DrmFourcc, PlaneLayout};
use super::hw::{AvFrame, BufferRef};
use crate::media::MediaError;
use crate::util::av::av_result;

/// Imports a dmabuf as a VA surface of `frames` without copying pixels.
pub(crate) fn map_dmabuf(
    frames: &BufferRef,
    frame: &DmabufFrame<'_>,
) -> Result<AvFrame, MediaError> {
    let descriptor = frame.drm_descriptor()?;
    let descriptor_buffer = BufferRef::from_raw(
        // SAFETY: av_buffer_allocz has no preconditions.
        unsafe { av_buffer_allocz(size_of::<AVDRMFrameDescriptor>()) },
        "av_buffer_allocz",
    )?;
    // SAFETY: the buffer holds exactly one zeroed, suitably aligned descriptor.
    unsafe {
        descriptor_buffer
            .data::<AVDRMFrameDescriptor>()
            .write(descriptor)
    };

    let mut source = AvFrame::new()?;
    let raw_source = source.get_mut();
    raw_source.format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
    raw_source.width = i32::try_from(frame.width).unwrap_or_default();
    raw_source.height = i32::try_from(frame.height).unwrap_or_default();
    raw_source.data[0] = descriptor_buffer.data::<u8>();
    raw_source.buf[0] = descriptor_buffer.into_raw();

    let mut surface = AvFrame::new()?;
    let raw_surface = surface.get_mut();
    raw_surface.format = AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
    raw_surface.hw_frames_ctx = frames.new_ref()?.into_raw();
    // SAFETY: surface names a VAAPI frames context and source a DRM PRIME descriptor frame.
    let code = unsafe {
        av_hwframe_map(
            surface.as_mut_ptr(),
            source.as_ptr(),
            AV_HWFRAME_MAP_READ as i32,
        )
    };
    av_result(code, "av_hwframe_map(drm -> vaapi)")?;
    Ok(surface)
}

/// A VA surface exported as a dmabuf. The export keeps the surface alive, so the fds stay
/// valid for as long as this value lives.
#[derive(Debug)]
pub struct ExportedDmabuf {
    mapping: AvFrame,
    fourcc: DrmFourcc,
    planes: [PlaneLayout; 2],
}

impl ExportedDmabuf {
    pub(crate) fn export(surface: &AvFrame) -> Result<Self, MediaError> {
        let mut mapping = AvFrame::new()?;
        mapping.get_mut().format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
        // SAFETY: surface is a live VAAPI frame and mapping an empty frame.
        let code = unsafe {
            av_hwframe_map(
                mapping.as_mut_ptr(),
                surface.as_ptr(),
                AV_HWFRAME_MAP_READ as i32,
            )
        };
        av_result(code, "av_hwframe_map(vaapi -> drm)")?;
        let (fourcc, planes) = flatten(descriptor(&mapping)?)?;
        Ok(Self {
            mapping,
            fourcc,
            planes,
        })
    }

    /// The export as a single-object dmabuf frame.
    pub fn frame(&self) -> Result<DmabufFrame<'_>, MediaError> {
        let descriptor = descriptor(&self.mapping)?;
        let object = &descriptor.objects[0];
        // SAFETY: the fd belongs to the export, which lives as long as the borrow of self.
        let fd = unsafe { BorrowedFd::borrow_raw(object.fd) };
        Ok(DmabufFrame {
            fd,
            fourcc: self.fourcc,
            modifier: object.format_modifier,
            width: u32::try_from(self.mapping.get().width).unwrap_or_default(),
            height: u32::try_from(self.mapping.get().height).unwrap_or_default(),
            planes: self
                .planes
                .get(..self.fourcc.plane_count())
                .unwrap_or_default(),
        })
    }
}

fn descriptor(mapping: &AvFrame) -> Result<&AVDRMFrameDescriptor, MediaError> {
    let pointer = mapping.get().data[0]
        .cast::<AVDRMFrameDescriptor>()
        .cast_const();
    // SAFETY: a DRM PRIME frame's data[0] points at its descriptor for the frame's lifetime.
    unsafe { pointer.as_ref() }.ok_or(MediaError::InvalidFrame("DRM export has no descriptor"))
}

const DRM_FORMAT_R8: u32 = u32::from_le_bytes(*b"R8  ");
const DRM_FORMAT_GR88: u32 = u32::from_le_bytes(*b"GR88");

/// Flattens an export into one format with its plane layouts, whether the driver reported one
/// multi-plane layer or one layer per plane.
fn flatten(descriptor: &AVDRMFrameDescriptor) -> Result<(DrmFourcc, [PlaneLayout; 2]), MediaError> {
    if descriptor.nb_objects != 1 {
        return Err(MediaError::Unsupported("multi-object dmabuf exports"));
    }
    let layer_count = usize::try_from(descriptor.nb_layers).unwrap_or_default();
    let formats = descriptor
        .layers
        .iter()
        .take(layer_count)
        .map(|layer| layer.format);
    let fourcc = match (formats.clone().next(), formats.clone().nth(1), layer_count) {
        (Some(format), None, 1) if format == DrmFourcc::Nv12.code() => DrmFourcc::Nv12,
        (Some(DRM_FORMAT_R8), Some(DRM_FORMAT_GR88), 2) => DrmFourcc::Nv12,
        (Some(format), None, 1) if format == DrmFourcc::Xrgb8888.code() => DrmFourcc::Xrgb8888,
        _ => return Err(MediaError::Unsupported("exported dmabuf format")),
    };
    let mut planes = descriptor
        .layers
        .iter()
        .take(layer_count)
        .flat_map(|layer| {
            let plane_count = usize::try_from(layer.nb_planes).unwrap_or_default();
            layer.planes.iter().take(plane_count)
        });
    let mut next = || {
        planes
            .next()
            .map(|plane| PlaneLayout {
                offset: u32::try_from(plane.offset).unwrap_or_default(),
                pitch: u32::try_from(plane.pitch).unwrap_or_default(),
            })
            .ok_or(MediaError::InvalidFrame("export lacks a plane"))
    };
    let first = next()?;
    let second = match fourcc {
        DrmFourcc::Nv12 => next()?,
        DrmFourcc::Xrgb8888 => PlaneLayout::default(),
    };
    Ok((fourcc, [first, second]))
}
