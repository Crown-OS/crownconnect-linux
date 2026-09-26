use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;

use gbm::{BufferObject, BufferObjectFlags, Format, Modifier};

use super::constraints::{ChosenLayout, IMPLICIT_MODIFIER};
use crate::media::video::{DmabufFrame, DrmFourcc, PlaneLayout};
use crate::wayland::WaylandError;

const DRI_DIRECTORY: &str = "/dev/dri";
const RENDER_NODE_PREFIX: &str = "renderD";

/// One capture buffer: a dmabuf the compositor renders into and the encoder imports.
#[derive(Debug)]
pub struct RingBuffer {
    fd: OwnedFd,
    fourcc: DrmFourcc,
    modifier: u64,
    width: u32,
    height: u32,
    planes: Vec<PlaneLayout>,
}

impl RingBuffer {
    /// The view `VideoEncoder::encode_dmabuf` takes; its identity stays the same for the life of
    /// the ring, so the encoder maps each slot once.
    pub fn dmabuf(&self) -> DmabufFrame<'_> {
        DmabufFrame {
            fd: self.fd.as_fd(),
            fourcc: self.fourcc,
            modifier: self.modifier,
            width: self.width,
            height: self.height,
            planes: &self.planes,
        }
    }

    pub(super) const fn modifier(&self) -> u64 {
        self.modifier
    }

    pub(super) fn planes(&self) -> impl Iterator<Item = (u32, &PlaneLayout)> {
        (0..).zip(&self.planes)
    }

    pub(super) const fn fd(&self) -> &OwnedFd {
        &self.fd
    }
}

/// The buffers of one constraints generation. A new generation means new buffers, and the
/// encoder should drop its mappings of the old ones.
#[derive(Debug)]
pub struct CaptureRing {
    pub generation: u64,
    pub fourcc: DrmFourcc,
    pub width: u32,
    pub height: u32,
    buffers: Vec<RingBuffer>,
}

impl CaptureRing {
    pub fn buffer(&self, slot: usize) -> Option<&RingBuffer> {
        self.buffers.get(slot)
    }

    pub(super) fn buffers(&self) -> impl Iterator<Item = (u32, &RingBuffer)> {
        (0..).zip(&self.buffers)
    }
}

/// Allocates capture rings on the first GPU render node through GBM.
pub(super) struct RingAllocator {
    device: gbm::Device<File>,
}

impl RingAllocator {
    pub(super) fn open() -> Result<Self, WaylandError> {
        let node = first_render_node()?;
        let file = File::options().read(true).write(true).open(&node)?;
        Ok(Self {
            device: gbm::Device::new(file)?,
        })
    }

    /// The GPU the ring is allocated on, which also hosts the session's syncobj timelines.
    pub(super) fn render_node(&self) -> BorrowedFd<'_> {
        self.device.as_fd()
    }

    pub(super) fn allocate(
        &self,
        generation: u64,
        layout: &ChosenLayout,
        size: (u32, u32),
        slots: usize,
    ) -> Result<CaptureRing, WaylandError> {
        let buffers = (0..slots)
            .map(|_| self.allocate_buffer(layout, size))
            .collect::<Result<_, _>>()?;
        Ok(CaptureRing {
            generation,
            fourcc: layout.fourcc,
            width: size.0,
            height: size.1,
            buffers,
        })
    }

    fn allocate_buffer(
        &self,
        layout: &ChosenLayout,
        (width, height): (u32, u32),
    ) -> Result<RingBuffer, WaylandError> {
        let format = Format::try_from(layout.fourcc.code())
            .map_err(|_| WaylandError::Allocation("unknown fourcc".into()))?;
        let usage = BufferObjectFlags::RENDERING;
        let object: BufferObject<()> = if layout.modifiers.is_empty() {
            self.device
                .create_buffer_object(width, height, format, usage)?
        } else {
            self.device.create_buffer_object_with_modifiers2(
                width,
                height,
                format,
                layout.modifiers.iter().copied().map(Modifier::from),
                usage,
            )?
        };
        let fd = object
            .fd()
            .map_err(|error| WaylandError::Allocation(error.to_string()))?;
        let planes = (0..i32::try_from(object.plane_count()).unwrap_or(0))
            .map(|plane| PlaneLayout {
                offset: object.offset(plane),
                pitch: object.stride_for_plane(plane),
            })
            .collect();
        let modifier = if layout.modifiers.is_empty() {
            IMPLICIT_MODIFIER
        } else {
            u64::from(object.modifier())
        };
        Ok(RingBuffer {
            fd,
            fourcc: layout.fourcc,
            modifier,
            width,
            height,
            planes,
        })
    }
}

fn first_render_node() -> Result<PathBuf, WaylandError> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir(DRI_DIRECTORY)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(RENDER_NODE_PREFIX))
        })
        .map(|entry| entry.path())
        .collect();
    nodes.sort_unstable();
    nodes
        .into_iter()
        .next()
        .ok_or(WaylandError::Missing("GPU render node"))
}
