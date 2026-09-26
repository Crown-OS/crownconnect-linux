//! Feeding a paired device's pointer, keyboard and touch into this computer's seat through the
//! crownos-input-v1 injector.

use std::sync::{Arc, Mutex};

use crownos_protocols::input::v1::client::crownos_input_injector_v1::{
    self, Axis, ButtonState, CrownosInputInjectorV1, KeyState,
};
use crownos_protocols::input::v1::client::crownos_input_manager_v1::{self, CrownosInputManagerV1};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle};

use super::input_types::{split_micros, ScrollAxis};
use super::outputs::{logical_point, track_outputs, Outputs};
use super::worker::{WorkerHandle, WorkerState};
use super::WaylandError;

/// One input event; absolute positions are fractions (0.0 to 1.0) of the target output, so the
/// sender needs no knowledge of its size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InjectedEvent {
    PointerMotion {
        dx: f64,
        dy: f64,
    },
    PointerTo {
        x: f64,
        y: f64,
    },
    Button {
        code: u32,
        pressed: bool,
    },
    Axis {
        axis: ScrollAxis,
        value: f64,
        discrete_120: i32,
    },
    Key {
        code: u32,
        pressed: bool,
    },
    TouchDown {
        id: i32,
        x: f64,
        y: f64,
    },
    TouchMotion {
        id: i32,
        x: f64,
        y: f64,
    },
    TouchUp {
        id: i32,
    },
    TouchCancel,
}

/// Events that belong to one hardware event, applied together.
#[derive(Debug, Clone, PartialEq)]
pub struct InputFrame {
    pub events: Vec<InjectedEvent>,
    pub time_us: u64,
}

/// The target output's logical size as the injector last saw it.
type SharedSize = Arc<Mutex<Option<(f64, f64)>>>;

#[derive(Debug)]
pub struct Injector {
    worker: WorkerHandle<InputFrame>,
    target_size: SharedSize,
}

impl Injector {
    /// Injects into the first seat; absolute events land on the output named `target`, or on
    /// the first output. Blocks for one roundtrip, so call it from a pipeline thread.
    ///
    /// # Errors
    ///
    /// Fails without a compositor offering crownos-input-v1 and a seat.
    pub fn connect(target: Option<String>) -> Result<Self, WaylandError> {
        let target_size = SharedSize::default();
        let shared = Arc::clone(&target_size);
        let worker =
            WorkerHandle::spawn_blocking("crownconnect-injector", move |globals, queue| {
                InjectorState::bind(globals, queue, target, shared)
            })?;
        Ok(Self {
            worker,
            target_size,
        })
    }

    /// The target output's logical size, once the first frame was injected.
    pub fn target_size(&self) -> Option<(f64, f64)> {
        self.target_size.lock().ok().and_then(|size| *size)
    }

    /// # Errors
    ///
    /// Fails once the injector's thread has stopped, as after the compositor revoked it.
    pub fn inject(&self, frame: InputFrame) -> Result<(), WaylandError> {
        self.worker.send(frame)
    }
}

struct InjectorState {
    outputs: Outputs,
    injector: CrownosInputInjectorV1,
    target: Option<String>,
    target_size: SharedSize,
    revoked: bool,
}

track_outputs!(InjectorState, outputs);

impl InjectorState {
    fn bind(
        globals: &GlobalList,
        queue: &QueueHandle<Self>,
        target: Option<String>,
        target_size: SharedSize,
    ) -> Result<Self, WaylandError> {
        let manager: CrownosInputManagerV1 = globals.bind(queue, 1..=1, ())?;
        let seat: WlSeat = globals.bind(queue, 1..=9, ())?;
        Ok(Self {
            outputs: Outputs::bind_existing(globals, queue),
            injector: manager.create_injector(&seat, queue, ()),
            target,
            target_size,
            revoked: false,
        })
    }

    fn inject(&self, event: InjectedEvent) {
        let target = self.outputs.find(self.target.as_deref());
        let absolute =
            |x, y| target.map(|output| (output, logical_point((x, y), output.logical_size())));
        let injector = &self.injector;
        match event {
            InjectedEvent::PointerMotion { dx, dy } => injector.pointer_motion(dx, dy),
            InjectedEvent::PointerTo { x, y } => {
                if let Some((output, (x, y))) = absolute(x, y) {
                    injector.pointer_motion_absolute(&output.output, x, y);
                }
            }
            InjectedEvent::Button { code, pressed } => injector.button(
                code,
                if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                },
            ),
            InjectedEvent::Axis {
                axis,
                value,
                discrete_120,
            } => injector.axis(
                match axis {
                    ScrollAxis::Vertical => Axis::VerticalScroll,
                    ScrollAxis::Horizontal => Axis::HorizontalScroll,
                },
                value,
                discrete_120,
            ),
            InjectedEvent::Key { code, pressed } => injector.key(
                code,
                if pressed {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                },
            ),
            InjectedEvent::TouchDown { id, x, y } => {
                if let Some((output, (x, y))) = absolute(x, y) {
                    injector.touch_down(id, &output.output, x, y);
                }
            }
            InjectedEvent::TouchMotion { id, x, y } => {
                if let Some((_, (x, y))) = absolute(x, y) {
                    injector.touch_motion(id, x, y);
                }
            }
            InjectedEvent::TouchUp { id } => injector.touch_up(id),
            InjectedEvent::TouchCancel => injector.touch_cancel(),
        }
    }
}

impl WorkerState for InjectorState {
    type Command = InputFrame;

    fn apply(&mut self, frame: InputFrame, _: &QueueHandle<Self>) {
        frame.events.iter().for_each(|event| self.inject(*event));
        let (hi, lo) = split_micros(frame.time_us);
        self.injector.frame(hi, lo);
        let size = self
            .outputs
            .find(self.target.as_deref())
            .map(|output| output.logical_size());
        if let Ok(mut shared) = self.target_size.lock() {
            *shared = size;
        }
    }

    fn is_finished(&self) -> bool {
        self.revoked
    }
}

impl Dispatch<CrownosInputInjectorV1, ()> for InjectorState {
    fn event(
        state: &mut Self,
        _: &CrownosInputInjectorV1,
        event: crownos_input_injector_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let crownos_input_injector_v1::Event::Revoked = event {
            state.revoked = true;
        }
    }
}

impl Dispatch<CrownosInputManagerV1, ()> for InjectorState {
    fn event(
        _: &mut Self,
        _: &CrownosInputManagerV1,
        _: crownos_input_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for InjectorState {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
