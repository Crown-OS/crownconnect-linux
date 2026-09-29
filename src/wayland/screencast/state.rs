use std::sync::Arc;
use std::time::Duration;

use crownos_protocols::screencast::v1::client::crownos_screencast_manager_v1::{
    self, Capability, CrownosScreencastManagerV1, CursorMode,
};
use crownos_protocols::screencast::v1::client::crownos_screencast_session_v1::{
    self, CrownosScreencastSessionV1, StopReason,
};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    self, Flags, ZwpLinuxBufferParamsV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::{
    self, ZwpLinuxDmabufV1,
};

use super::constraints::BufferConstraints;
use super::damage::{DamageCollector, DamageRect};
use super::ring::{CaptureRing, RingAllocator};
use super::sync::ExplicitSync;
use super::{
    CapturedFrame, CursorDelivery, CursorImage, CursorUpdate, ScreencastEvent, ScreencastOptions,
    StopCause,
};
use crate::wayland::outputs::{track_outputs, Outputs};
use crate::wayland::worker::WorkerState;
use crate::wayland::{EventOutlet, WaylandError};

const MAX_RING_SLOTS: usize = 4;
const DMABUF_VERSION: u32 = 3;

#[derive(Debug)]
pub(super) enum SessionCommand {
    Begin,
    Release { slot: usize, generation: u64 },
    ForceFrame,
    Stop,
}

pub(super) struct ScreencastState {
    outputs: Outputs,
    manager: CrownosScreencastManagerV1,
    dmabuf: ZwpLinuxDmabufV1,
    allocator: RingAllocator,
    options: ScreencastOptions,
    events: EventOutlet<ScreencastEvent>,
    session: Option<CrownosScreencastSessionV1>,
    explicit_sync_offered: bool,
    sync: Option<ExplicitSync>,
    constraints: BufferConstraints,
    ring: Option<Arc<CaptureRing>>,
    attached: Vec<WlBuffer>,
    started: bool,
    damage: DamageCollector,
    cursor_image: Option<CursorImage>,
    finished: bool,
}

track_outputs!(ScreencastState, outputs);

impl ScreencastState {
    pub(super) fn bind(
        globals: &GlobalList,
        queue: &QueueHandle<Self>,
        options: ScreencastOptions,
        events: EventOutlet<ScreencastEvent>,
    ) -> Result<Self, WaylandError> {
        Ok(Self {
            manager: globals.bind(queue, 1..=1, ())?,
            dmabuf: globals.bind(queue, DMABUF_VERSION..=DMABUF_VERSION, ())?,
            outputs: Outputs::bind_existing(globals, queue),
            allocator: RingAllocator::open()?,
            options,
            events,
            session: None,
            explicit_sync_offered: false,
            sync: None,
            constraints: BufferConstraints::default(),
            ring: None,
            attached: Vec::new(),
            started: false,
            damage: DamageCollector::default(),
            cursor_image: None,
            finished: false,
        })
    }

    fn begin(&mut self, queue: &QueueHandle<Self>) {
        let Some(output) = self.outputs.find(self.options.output.as_deref()) else {
            self.stop_with(StopCause::OutputNotFound);
            return;
        };
        let cursor = match self.options.cursor {
            CursorDelivery::Hidden => CursorMode::Hidden,
            CursorDelivery::Embedded => CursorMode::Embedded,
            CursorDelivery::Metadata => CursorMode::Metadata,
        };
        let session = self
            .manager
            .create_output_session(&output.output, cursor, queue, ());
        self.sync = self.attach_timelines(&session);
        self.session = Some(session);
    }

    fn attach_timelines(&self, session: &CrownosScreencastSessionV1) -> Option<ExplicitSync> {
        if !self.explicit_sync_offered {
            return None;
        }
        ExplicitSync::attach(self.allocator.render_node(), session)
            .inspect_err(
                |error| tracing::warn!(%error, "explicit sync unavailable; using implicit"),
            )
            .ok()
    }

    fn emit(&self, event: ScreencastEvent) -> Result<(), ScreencastEvent> {
        (self.events)(event)
    }

    fn stop_with(&mut self, cause: StopCause) {
        let _ = self.emit(ScreencastEvent::Stopped(cause));
        if let Some(session) = self.session.take() {
            session.destroy();
        }
        self.finished = true;
    }

    fn reallocate(&mut self, queue: &QueueHandle<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let candidates = self.constraints.candidates();
        if candidates.is_empty() {
            self.stop_with(StopCause::NoUsableFormat);
            return;
        }
        let generation = self.ring.as_ref().map_or(0, |ring| ring.generation + 1);
        let slots = self.options.ring_slots.clamp(1, MAX_RING_SLOTS);
        let size = (self.constraints.width, self.constraints.height);
        let allocated = candidates.iter().find_map(|layout| {
            self.allocator
                .allocate(generation, layout, size, slots)
                .inspect_err(|error| {
                    tracing::debug!(%error, fourcc = ?layout.fourcc, "layout not allocatable; trying the next");
                })
                .ok()
        });
        let Some(ring) = allocated.map(Arc::new) else {
            tracing::warn!(
                width = size.0,
                height = size.1,
                "cannot allocate the capture ring in any offered layout"
            );
            self.stop_with(StopCause::AllocationFailed);
            return;
        };
        let previous = std::mem::take(&mut self.attached);
        for (index, buffer) in ring.buffers() {
            let wl_buffer = self.import(&ring, buffer, queue);
            session.attach_buffer(index, &wl_buffer);
            self.attached.push(wl_buffer);
        }
        previous.iter().for_each(WlBuffer::destroy);
        if !self.started {
            session.start(self.options.max_fps.saturating_mul(1_000));
            self.started = true;
        }
        self.ring = Some(Arc::clone(&ring));
        let _ = self.emit(ScreencastEvent::Ring(ring));
    }

    fn import(
        &self,
        ring: &CaptureRing,
        buffer: &super::RingBuffer,
        queue: &QueueHandle<Self>,
    ) -> WlBuffer {
        let params = self.dmabuf.create_params(queue, ());
        let [modifier_hi, modifier_lo] = split_u64(buffer.modifier());
        for (plane, layout) in buffer.planes() {
            params.add(
                std::os::fd::AsFd::as_fd(buffer.fd()),
                plane,
                layout.offset,
                layout.pitch,
                modifier_hi,
                modifier_lo,
            );
        }
        let wl_buffer = params.create_immed(
            i32::try_from(ring.width).unwrap_or(i32::MAX),
            i32::try_from(ring.height).unwrap_or(i32::MAX),
            ring.fourcc.code(),
            Flags::empty(),
            queue,
            (),
        );
        params.destroy();
        wl_buffer
    }

    fn deliver(&mut self, slot: u32, pts: Duration, acquire_point: u64) {
        let Some(ring) = self.ring.clone() else {
            return;
        };
        let frame = CapturedFrame {
            slot: usize::try_from(slot).unwrap_or(usize::MAX),
            ring,
            damage: self.damage.take(),
            pts,
            acquire: self
                .sync
                .as_ref()
                .filter(|_| acquire_point != 0)
                .map(|sync| sync.acquire_point(acquire_point)),
        };
        if self.emit(ScreencastEvent::Frame(frame)).is_err() {
            self.release_slot(slot);
        }
    }

    fn release(&mut self, slot: usize, generation: u64) {
        let current = self
            .ring
            .as_ref()
            .is_some_and(|ring| ring.generation == generation);
        if let (true, Ok(slot)) = (current, u32::try_from(slot)) {
            self.release_slot(slot);
        }
    }

    fn release_slot(&mut self, slot: u32) {
        if self.session.is_none() {
            return;
        }
        let release_point = match self.sync.as_mut().map(ExplicitSync::signal_release) {
            None => 0,
            Some(Ok(point)) => point,
            Some(Err(error)) => {
                tracing::warn!(%error, "cannot signal a capture release point");
                self.stop_with(StopCause::RenderFailed);
                return;
            }
        };
        if let Some(session) = &self.session {
            let [hi, lo] = split_u64(release_point);
            session.release(slot, hi, lo);
        }
    }
}

impl WorkerState for ScreencastState {
    type Command = SessionCommand;

    fn apply(&mut self, command: SessionCommand, queue: &QueueHandle<Self>) {
        match command {
            SessionCommand::Begin => self.begin(queue),
            SessionCommand::Release { slot, generation } => self.release(slot, generation),
            SessionCommand::ForceFrame => {
                if let Some(session) = &self.session {
                    session.force_frame();
                }
            }
            SessionCommand::Stop => {
                if let Some(session) = &self.session {
                    session.stop();
                }
            }
        }
    }

    fn is_finished(&self) -> bool {
        self.finished
    }
}

const fn split_u64(value: u64) -> [u32; 2] {
    let [hi, lo] = [value >> 32, value & 0xffff_ffff];
    #[expect(clippy::cast_possible_truncation, reason = "each half fits in 32 bits")]
    [hi as u32, lo as u32]
}

const fn join_u32(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

const fn stop_cause(reason: StopReason) -> StopCause {
    match reason {
        StopReason::Requested => StopCause::Requested,
        StopReason::SourceDestroyed => StopCause::SourceDestroyed,
        StopReason::Revoked => StopCause::Revoked,
        _ => StopCause::RenderFailed,
    }
}

impl Dispatch<CrownosScreencastSessionV1, ()> for ScreencastState {
    fn event(
        state: &mut Self,
        _: &CrownosScreencastSessionV1,
        event: crownos_screencast_session_v1::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        use crownos_screencast_session_v1::Event;
        match event {
            Event::Constraints { width, height } => state.constraints.begin(width, height),
            Event::Format { format } => state.constraints.add_format(format),
            Event::Modifier {
                format,
                modifier_hi,
                modifier_lo,
            } => state
                .constraints
                .add_modifier(format, modifier_hi, modifier_lo),
            Event::ConstraintsDone => state.reallocate(queue),
            Event::Damage {
                x,
                y,
                width,
                height,
            } => state.damage.add(DamageRect {
                x,
                y,
                width,
                height,
            }),
            Event::Frame {
                index,
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                acquire_point_hi,
                acquire_point_lo,
            } => state.deliver(
                index,
                Duration::new(join_u32(tv_sec_hi, tv_sec_lo), tv_nsec),
                join_u32(acquire_point_hi, acquire_point_lo),
            ),
            Event::CursorPosition {
                x,
                y,
                hotspot_x,
                hotspot_y,
            } => {
                let _ = state.emit(ScreencastEvent::Cursor(CursorUpdate::Moved {
                    x,
                    y,
                    hotspot_x,
                    hotspot_y,
                }));
            }
            Event::CursorLeave => {
                let _ = state.emit(ScreencastEvent::Cursor(CursorUpdate::Left));
            }
            Event::CursorBuffer {
                fd,
                width,
                height,
                stride,
            } => {
                state.cursor_image = Some(CursorImage {
                    pixels: fd,
                    width,
                    height,
                    stride,
                });
            }
            Event::CursorShape {
                serial,
                hotspot_x,
                hotspot_y,
            } => {
                let image = state.cursor_image.take();
                let _ = state.emit(ScreencastEvent::Cursor(CursorUpdate::Shape {
                    serial,
                    hotspot_x,
                    hotspot_y,
                    image,
                }));
            }
            Event::Stopped { reason } => {
                let cause = match reason {
                    WEnum::Value(reason) => stop_cause(reason),
                    WEnum::Unknown(_) => StopCause::RenderFailed,
                };
                state.stop_with(cause);
            }
            _ => {}
        }
    }
}

impl Dispatch<CrownosScreencastManagerV1, ()> for ScreencastState {
    fn event(
        state: &mut Self,
        _: &CrownosScreencastManagerV1,
        event: crownos_screencast_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let crownos_screencast_manager_v1::Event::Capabilities { capabilities } = event {
            state.explicit_sync_offered = matches!(
                capabilities,
                WEnum::Value(capabilities) if capabilities.contains(Capability::ExplicitSync)
            );
        }
    }
}

impl Dispatch<ZwpLinuxDmabufV1, ()> for ScreencastState {
    fn event(
        _: &mut Self,
        _: &ZwpLinuxDmabufV1,
        _: zwp_linux_dmabuf_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpLinuxBufferParamsV1, ()> for ScreencastState {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_linux_buffer_params_v1::Event::Failed = event {
            state.stop_with(StopCause::AllocationFailed);
        }
    }
}

impl Dispatch<WlBuffer, ()> for ScreencastState {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixty_four_bit_values_split_and_join_losslessly() {
        let value = 0x0123_4567_89ab_cdef;
        let [hi, lo] = split_u64(value);
        assert_eq!(join_u32(hi, lo), value);
    }

    #[test]
    fn compositor_stop_reasons_map_onto_causes() {
        assert_eq!(stop_cause(StopReason::Revoked), StopCause::Revoked);
        assert_eq!(
            stop_cause(StopReason::RenderFailed),
            StopCause::RenderFailed
        );
    }
}
