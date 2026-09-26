use wayland_client::protocol::wl_keyboard::{self, KeyState, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, Axis, WlPointer};
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_touch::{self, WlTouch};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};

use super::state::{button_pressed, State};
use crate::media::present::input::{InputEvent, InputMessage};
use crate::util::numeric::{narrow_f32, saturating_i32};

/// Continuous scrolling (touchpads) in surface pixels per wheel detent, as libinput reports it.
const PIXELS_PER_DETENT: f64 = 15.0;
const V120_PER_DETENT: f64 = 120.0;

impl State {
    fn push(&mut self, time_ms: u32, event: InputEvent) {
        self.input.push(InputMessage { time_ms, event });
    }

    fn normalised(&self, x: f64, y: f64) -> (f32, f32) {
        let (width, height) = self.content_size;
        let fraction = |value: f64, extent: i32| {
            narrow_f32((value / f64::from(extent.max(1))).clamp(0.0, 1.0))
        };
        (fraction(x, width), fraction(y, height))
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        if capabilities.contains(Capability::Pointer) {
            seat.get_pointer(queue, ());
        }
        if capabilities.contains(Capability::Keyboard) {
            seat.get_keyboard(queue, ());
        }
        if capabilities.contains(Capability::Touch) {
            seat.get_touch(queue, ());
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                let (x, y) = state.normalised(surface_x, surface_y);
                state.push(0, InputEvent::PointerMotion { x, y });
            }
            wl_pointer::Event::Motion {
                time,
                surface_x,
                surface_y,
            } => {
                let (x, y) = state.normalised(surface_x, surface_y);
                state.push(time, InputEvent::PointerMotion { x, y });
            }
            wl_pointer::Event::Button {
                time,
                button,
                state: pressed,
                ..
            } => {
                state.push(
                    time,
                    InputEvent::PointerButton {
                        button,
                        pressed: button_pressed(pressed),
                    },
                );
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(axis),
                value,
                ..
            } => {
                accumulate(
                    state,
                    axis,
                    saturating_i32(value / PIXELS_PER_DETENT * V120_PER_DETENT),
                );
            }
            wl_pointer::Event::AxisValue120 {
                axis: WEnum::Value(axis),
                value120,
            } => {
                state.scroll_v120 = (0, 0);
                accumulate(state, axis, value120);
            }
            wl_pointer::Event::Frame => {
                let (horizontal_v120, vertical_v120) = std::mem::take(&mut state.scroll_v120);
                if (horizontal_v120, vertical_v120) != (0, 0) {
                    state.push(
                        0,
                        InputEvent::Scroll {
                            horizontal_v120,
                            vertical_v120,
                        },
                    );
                }
                state.push(0, InputEvent::Frame);
            }
            _ => {}
        }
    }
}

const fn accumulate(state: &mut State, axis: Axis, v120: i32) {
    match axis {
        Axis::HorizontalScroll => state.scroll_v120.0 += v120,
        Axis::VerticalScroll => state.scroll_v120.1 += v120,
        _ => {}
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            time,
            key,
            state: key_state,
            ..
        } = event
        {
            let pressed = matches!(key_state, WEnum::Value(KeyState::Pressed));
            state.push(time, InputEvent::Key { code: key, pressed });
        }
    }
}

impl Dispatch<WlTouch, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down { time, id, x, y, .. } => {
                let (x, y) = state.normalised(x, y);
                state.push(time, InputEvent::TouchDown { id, x, y });
            }
            wl_touch::Event::Motion { time, id, x, y } => {
                let (x, y) = state.normalised(x, y);
                state.push(time, InputEvent::TouchMotion { id, x, y });
            }
            wl_touch::Event::Up { time, id, .. } => state.push(time, InputEvent::TouchUp { id }),
            wl_touch::Event::Frame => state.push(0, InputEvent::Frame),
            wl_touch::Event::Cancel => state.push(0, InputEvent::TouchCancel),
            _ => {}
        }
    }
}
