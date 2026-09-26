//! Zero-copy screen capture through crownos-screencast-v1: a ring of GBM dmabufs the compositor
//! renders into, handed out one frame at a time and paced by how fast they come back.
//!
//! When the compositor offers explicit sync, the session hands it two DRM syncobj timelines: a
//! frame may arrive before rendering finished and carries an [`AcquirePoint`] to wait on, and a
//! release signals the next point on the release timeline. Otherwise synchronisation is implicit
//! and a frame arrives once the GPU finished rendering it.

mod constraints;
mod damage;
mod ring;
mod state;
mod sync;

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::Duration;

use super::worker::WorkerHandle;
use super::{EventOutlet, WaylandError};
use crate::media::video::DmabufFrame;
pub use damage::DamageRect;
pub use ring::{CaptureRing, RingBuffer};
use state::{ScreencastState, SessionCommand};
pub use sync::AcquirePoint;

use crate::util::syncobj::SyncobjError;

/// Enough for one frame being encoded, one being rendered and one spare, well within the
/// protocol's maximum of four.
pub const DEFAULT_RING_SLOTS: usize = 3;
/// Far beyond any healthy render; a frame not ready by then is dropped rather than encoded.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_millis(100);

/// How the pointer appears in the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorDelivery {
    Hidden,
    Embedded,
    /// Left out of frames and reported as [`CursorUpdate`]s, for the viewer to draw.
    Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreencastOptions {
    /// The output to capture by its wl_output name; the first output when `None`.
    pub output: Option<String>,
    pub cursor: CursorDelivery,
    /// Frame-rate cap; zero follows the output's refresh.
    pub max_fps: u32,
    pub ring_slots: usize,
}

/// A captured frame, owned by the caller until it is handed back with
/// [`Screencast::release`].
#[derive(Debug)]
pub struct CapturedFrame {
    pub slot: usize,
    pub ring: Arc<CaptureRing>,
    pub damage: Vec<DamageRect>,
    /// CLOCK_MONOTONIC time the content was sampled at.
    pub pts: Duration,
    /// Where the frame is ready under explicit sync; `None` means it already is.
    pub acquire: Option<AcquirePoint>,
}

impl CapturedFrame {
    pub fn dmabuf(&self) -> Option<DmabufFrame<'_>> {
        self.ring.buffer(self.slot).map(RingBuffer::dmabuf)
    }

    /// Blocks until the compositor finished rendering the frame, for at most
    /// [`ACQUIRE_TIMEOUT`]. Call it on the encoder's thread right before submitting the dmabuf.
    ///
    /// # Errors
    ///
    /// Fails when the frame is not ready in time; release it without encoding.
    pub fn wait_rendered(&self) -> Result<(), SyncobjError> {
        self.acquire
            .as_ref()
            .map_or(Ok(()), |acquire| acquire.wait(ACQUIRE_TIMEOUT))
    }
}

#[derive(Debug)]
pub struct CursorImage {
    /// Sealed memfd with premultiplied ARGB8888 pixels.
    pub pixels: OwnedFd,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

#[derive(Debug)]
pub enum CursorUpdate {
    Moved {
        x: i32,
        y: i32,
        hotspot_x: i32,
        hotspot_y: i32,
    },
    Left,
    Shape {
        serial: u32,
        hotspot_x: i32,
        hotspot_y: i32,
        /// `None` when the cursor was hidden.
        image: Option<CursorImage>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    Requested,
    SourceDestroyed,
    Revoked,
    RenderFailed,
    OutputNotFound,
    NoUsableFormat,
    AllocationFailed,
}

#[derive(Debug)]
pub enum ScreencastEvent {
    /// Buffers were (re)allocated; frames from an older ring stay readable but mappings of it
    /// can be dropped.
    Ring(Arc<CaptureRing>),
    Frame(CapturedFrame),
    Cursor(CursorUpdate),
    Stopped(StopCause),
}

/// A running capture session on its own wayland thread.
#[derive(Debug)]
pub struct Screencast {
    worker: WorkerHandle<SessionCommand>,
}

impl Screencast {
    /// Starts capturing and hands every event to `events`; frames it gives back are released.
    /// Blocks for one roundtrip, so call it from a pipeline thread, never the async runtime.
    ///
    /// # Errors
    ///
    /// Fails without a compositor offering crownos-screencast-v1 and linux-dmabuf, or without a
    /// GPU render node to allocate from.
    pub fn start(
        options: ScreencastOptions,
        events: EventOutlet<ScreencastEvent>,
    ) -> Result<Self, WaylandError> {
        let worker =
            WorkerHandle::spawn_blocking("crownconnect-screencast", move |globals, queue| {
                ScreencastState::bind(globals, queue, options, events)
            })?;
        worker.send(SessionCommand::Begin)?;
        Ok(Self { worker })
    }

    /// Gives a frame's slot back so the compositor can render into it again. Call it once the
    /// encoder is done reading the frame, i.e. after its packet came out of the encoder; under
    /// explicit sync this signals the next point on the release timeline.
    ///
    /// # Errors
    ///
    /// Fails once the session's thread has stopped.
    pub fn release(&self, frame: CapturedFrame) -> Result<(), WaylandError> {
        let CapturedFrame { slot, ring, .. } = frame;
        self.worker.send(SessionCommand::Release {
            slot,
            generation: ring.generation,
        })
    }

    /// Asks for a complete frame, as a keyframe request needs while the screen is static.
    ///
    /// # Errors
    ///
    /// Fails once the session's thread has stopped.
    pub fn force_frame(&self) -> Result<(), WaylandError> {
        self.worker.send(SessionCommand::ForceFrame)
    }

    /// # Errors
    ///
    /// Fails once the session's thread has stopped.
    pub fn stop(&self) -> Result<(), WaylandError> {
        self.worker.send(SessionCommand::Stop)
    }
}
