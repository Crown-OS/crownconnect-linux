use std::os::fd::{AsRawFd, BorrowedFd};

use ffmpeg_next::ffi::AVDRMFrameDescriptor;
use rustix::fs::{fstat, seek, SeekFrom};

use crate::media::MediaError;

/// The DRM formats a captured frame may arrive in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DrmFourcc {
    Nv12,
    Xrgb8888,
}

impl DrmFourcc {
    pub const fn code(self) -> u32 {
        match self {
            Self::Nv12 => u32::from_le_bytes(*b"NV12"),
            Self::Xrgb8888 => u32::from_le_bytes(*b"XR24"),
        }
    }

    pub const fn plane_count(self) -> usize {
        match self {
            Self::Nv12 => 2,
            Self::Xrgb8888 => 1,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlaneLayout {
    pub offset: u32,
    pub pitch: u32,
}

/// A single-object dmabuf frame, as a compositor's capture ring hands it out.
#[derive(Debug, Clone, Copy)]
pub struct DmabufFrame<'fd> {
    pub fd: BorrowedFd<'fd>,
    pub fourcc: DrmFourcc,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    pub planes: &'fd [PlaneLayout],
}

/// What makes two dmabuf frames the same buffer, so a mapping made for one serves the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferIdentity {
    pub device: u64,
    pub inode: u64,
    pub fourcc: DrmFourcc,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
}

impl DmabufFrame<'_> {
    pub fn identity(&self) -> Result<BufferIdentity, MediaError> {
        let stat = fstat(self.fd)?;
        Ok(BufferIdentity {
            device: stat.st_dev,
            inode: stat.st_ino,
            fourcc: self.fourcc,
            modifier: self.modifier,
            width: self.width,
            height: self.height,
        })
    }

    pub(crate) fn drm_descriptor(&self) -> Result<AVDRMFrameDescriptor, MediaError> {
        if self.planes.len() != self.fourcc.plane_count() {
            return Err(MediaError::InvalidFrame(
                "plane count does not match the fourcc",
            ));
        }
        let object_size = seek(self.fd, SeekFrom::End(0))?;
        // SAFETY: AVDRMFrameDescriptor is plain old data for which all-zero is valid.
        let mut descriptor: AVDRMFrameDescriptor = unsafe { std::mem::zeroed() };
        descriptor.nb_objects = 1;
        descriptor.objects[0].fd = self.fd.as_raw_fd();
        descriptor.objects[0].size = usize::try_from(object_size).unwrap_or_default();
        descriptor.objects[0].format_modifier = self.modifier;
        descriptor.nb_layers = 1;
        let layer = &mut descriptor.layers[0];
        layer.format = self.fourcc.code();
        layer.nb_planes = 0;
        for (target, plane) in layer.planes.iter_mut().zip(self.planes) {
            target.object_index = 0;
            target.offset = isize::try_from(plane.offset).unwrap_or_default();
            target.pitch = isize::try_from(plane.pitch).unwrap_or_default();
            layer.nb_planes += 1;
        }
        Ok(descriptor)
    }
}
