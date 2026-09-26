//! Unicursor: taking over the local pointer and keyboard once the cursor crosses an armed edge
//! (crownos-input-v1 capture), so their events can be forwarded to a paired device.

use crownos_protocols::input::v1::client::crownos_input_capture_v1::{
    self, Axis, ButtonState, CrownosInputCaptureV1, KeyState,
};
use crownos_protocols::input::v1::client::crownos_input_manager_v1::{self, CrownosInputManagerV1};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};

use super::input_types::{edge_bits, edge_from_bits, join_micros, ScrollAxis};
use super::outputs::{logical_point, track_outputs, Outputs};
use super::worker::{WorkerHandle, WorkerState};
use super::{EventOutlet, WaylandError};
use crate::ipc::proto::Edge;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureEvent {
    /// Input now goes to the capture; `position` is where along `edge` the pointer crossed, as a
    /// fraction of that edge from its left or top end.
    Entered {
        edge: Edge,
        position: f64,
    },
    Motion {
        dx: f64,
        dy: f64,
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
    Modifiers {
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    },
    Frame {
        time_us: u64,
    },
    /// Input is back with the local seat.
    Released,
}

#[derive(Debug)]
enum CaptureCommand {
    Arm(u32),
    Release {
        output: Option<String>,
        x: f64,
        y: f64,
    },
}

#[derive(Debug)]
pub struct InputCapture {
    worker: WorkerHandle<CaptureCommand>,
}

impl InputCapture {
    /// Binds a capture of the first seat, reporting to `events`. Blocks for one roundtrip, so
    /// call it from a pipeline thread.
    ///
    /// # Errors
    ///
    /// Fails without a compositor offering crownos-input-v1 and a seat.
    pub fn connect(events: EventOutlet<CaptureEvent>) -> Result<Self, WaylandError> {
        let worker =
            WorkerHandle::spawn_blocking("crownconnect-capture", move |globals, queue| {
                CaptureState::bind(globals, queue, events)
            })?;
        Ok(Self { worker })
    }

    /// Replaces the armed edges; an empty slice disarms.
    ///
    /// # Errors
    ///
    /// Fails once the capture's thread has stopped.
    pub fn arm(&self, edges: &[Edge]) -> Result<(), WaylandError> {
        self.worker.send(CaptureCommand::Arm(edge_bits(edges)))
    }

    /// Hands input back, putting the pointer at the given fractions of `output` (the first
    /// output when `None`).
    ///
    /// # Errors
    ///
    /// Fails once the capture's thread has stopped.
    pub fn release(&self, output: Option<String>, x: f64, y: f64) -> Result<(), WaylandError> {
        self.worker.send(CaptureCommand::Release { output, x, y })
    }
}

struct CaptureState {
    outputs: Outputs,
    capture: CrownosInputCaptureV1,
    events: EventOutlet<CaptureEvent>,
}

track_outputs!(CaptureState, outputs);

impl CaptureState {
    fn bind(
        globals: &GlobalList,
        queue: &QueueHandle<Self>,
        events: EventOutlet<CaptureEvent>,
    ) -> Result<Self, WaylandError> {
        let manager: CrownosInputManagerV1 = globals.bind(queue, 1..=1, ())?;
        let seat: WlSeat = globals.bind(queue, 1..=9, ())?;
        Ok(Self {
            outputs: Outputs::bind_existing(globals, queue),
            capture: manager.create_capture(&seat, queue, ()),
            events,
        })
    }
}

impl CaptureState {
    fn edge_length(
        &self,
        output: &wayland_client::protocol::wl_output::WlOutput,
        edge: WEnum<crownos_input_capture_v1::Edge>,
    ) -> f64 {
        let Some((width, height)) = self
            .outputs
            .find_output(output)
            .map(|tracked| tracked.logical_size())
        else {
            return 1.0;
        };
        let length = match edge {
            WEnum::Value(edge)
                if edge.intersects(
                    crownos_input_capture_v1::Edge::Top | crownos_input_capture_v1::Edge::Bottom,
                ) =>
            {
                width
            }
            _ => height,
        };
        length.max(1.0)
    }
}

impl WorkerState for CaptureState {
    type Command = CaptureCommand;

    fn apply(&mut self, command: CaptureCommand, _: &QueueHandle<Self>) {
        match command {
            CaptureCommand::Arm(bits) => self
                .capture
                .arm(crownos_input_capture_v1::Edge::from_bits_truncate(bits)),
            CaptureCommand::Release { output, x, y } => {
                if let Some(output) = self.outputs.find(output.as_deref()) {
                    let (x, y) = logical_point((x, y), output.logical_size());
                    self.capture.release(&output.output, x, y);
                }
            }
        }
    }
}

const fn pressed(state: WEnum<ButtonState>) -> bool {
    matches!(state, WEnum::Value(ButtonState::Pressed))
}

const fn key_pressed(state: WEnum<KeyState>) -> bool {
    matches!(state, WEnum::Value(KeyState::Pressed))
}

const fn scroll_axis(axis: WEnum<Axis>) -> ScrollAxis {
    match axis {
        WEnum::Value(Axis::HorizontalScroll) => ScrollAxis::Horizontal,
        _ => ScrollAxis::Vertical,
    }
}

/// `edge_length` is the logical length of the edge an `entered` event names, which turns its
/// position into a fraction.
fn capture_event(
    event: &crownos_input_capture_v1::Event,
    edge_length: f64,
) -> Option<CaptureEvent> {
    use crownos_input_capture_v1::Event;
    Some(match *event {
        Event::Entered { edge, position, .. } => {
            let bits = match edge {
                WEnum::Value(edge) => edge.bits(),
                WEnum::Unknown(bits) => bits,
            };
            CaptureEvent::Entered {
                edge: edge_from_bits(bits)?,
                position: (position / edge_length).clamp(0.0, 1.0),
            }
        }
        Event::Motion { dx, dy } => CaptureEvent::Motion { dx, dy },
        Event::Button { button, state } => CaptureEvent::Button {
            code: button,
            pressed: pressed(state),
        },
        Event::Axis {
            axis,
            value,
            value120,
        } => CaptureEvent::Axis {
            axis: scroll_axis(axis),
            value,
            discrete_120: value120,
        },
        Event::Key { key, state } => CaptureEvent::Key {
            code: key,
            pressed: key_pressed(state),
        },
        Event::Modifiers {
            depressed,
            latched,
            locked,
            group,
        } => CaptureEvent::Modifiers {
            depressed,
            latched,
            locked,
            group,
        },
        Event::Frame {
            time_usec_hi,
            time_usec_lo,
        } => CaptureEvent::Frame {
            time_us: join_micros(time_usec_hi, time_usec_lo),
        },
        Event::Released => CaptureEvent::Released,
        _ => return None,
    })
}

impl Dispatch<CrownosInputCaptureV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &CrownosInputCaptureV1,
        event: crownos_input_capture_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let edge_length = match &event {
            crownos_input_capture_v1::Event::Entered { output, edge, .. } => {
                state.edge_length(output, *edge)
            }
            _ => 1.0,
        };
        if let Some(event) = capture_event(&event, edge_length)
            && (state.events)(event).is_err()
        {
            tracing::warn!("dropping a captured input event");
        }
    }
}

impl Dispatch<CrownosInputManagerV1, ()> for CaptureState {
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

impl Dispatch<WlSeat, ()> for CaptureState {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_events_map_onto_capture_events() {
        use crownos_input_capture_v1::Event;
        assert_eq!(
            capture_event(
                &Event::Button {
                    button: 0x110,
                    state: WEnum::Value(ButtonState::Pressed),
                },
                1.0
            ),
            Some(CaptureEvent::Button {
                code: 0x110,
                pressed: true
            })
        );
        assert_eq!(
            capture_event(
                &Event::Axis {
                    axis: WEnum::Value(Axis::HorizontalScroll),
                    value: 1.5,
                    value120: 120,
                },
                1.0
            ),
            Some(CaptureEvent::Axis {
                axis: ScrollAxis::Horizontal,
                value: 1.5,
                discrete_120: 120
            })
        );
        assert_eq!(
            capture_event(
                &Event::Frame {
                    time_usec_hi: 0,
                    time_usec_lo: 42
                },
                1.0
            ),
            Some(CaptureEvent::Frame { time_us: 42 })
        );
        assert_eq!(
            capture_event(&Event::Released, 1.0),
            Some(CaptureEvent::Released)
        );
    }
}
