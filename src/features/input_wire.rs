//! Remote input on the wire: each input-stream batch is the viewer's framed `InputMessage`s
//! (`[len: u8][postcard]` each), ending with a `Frame` event, so a batch is one moment of input.
//! A key-state snapshot is a batch of `Key { pressed: true }` messages, one per held key.

use std::collections::BTreeSet;

use crate::media::present::input::{read_message, write_message, InputEvent, InputMessage};
use crate::wayland::injector::{InjectedEvent, InputFrame};
use crate::wayland::ScrollAxis;

/// A wheel detent in `wl_pointer.axis` units, the scale the injector's axis value uses.
const SCROLL_UNITS_PER_DETENT: f64 = 15.0;
const V120_PER_DETENT: f64 = 120.0;
const MICROS_PER_MILLI: u64 = 1_000;

/// Collects one moment of input into a wire batch.
#[derive(Debug, Default)]
pub(crate) struct BatchWriter {
    bytes: Vec<u8>,
}

impl BatchWriter {
    pub(crate) fn push(&mut self, time_ms: u32, event: InputEvent) {
        if let Err(error) = write_message(&mut self.bytes, &InputMessage { time_ms, event }) {
            tracing::debug!(%error, "unencodable input event");
        }
    }

    /// The finished batch, if anything was pushed since the last one.
    pub(crate) fn take(&mut self) -> Option<Vec<u8>> {
        (!self.bytes.is_empty()).then(|| std::mem::take(&mut self.bytes))
    }
}

/// Every message of a batch, stopping at the first malformed one.
pub(crate) fn messages(mut batch: &[u8]) -> impl Iterator<Item = InputMessage> + '_ {
    std::iter::from_fn(move || read_message(&mut batch).ok().flatten())
}

/// Splits a batch into injector frames, one per `Frame` event (and one for a trailing group).
pub(crate) fn input_frames(batch: &[u8]) -> Vec<InputFrame> {
    let mut frames = Vec::new();
    let mut current = InputFrame {
        events: Vec::new(),
        time_us: 0,
    };
    for message in messages(batch) {
        current.time_us = u64::from(message.time_ms) * MICROS_PER_MILLI;
        match message.event {
            InputEvent::Frame => frames.push(std::mem::replace(
                &mut current,
                InputFrame {
                    events: Vec::new(),
                    time_us: 0,
                },
            )),
            event => current.events.extend(injected(event)),
        }
    }
    if !current.events.is_empty() {
        frames.push(current);
    }
    frames
}

/// What an input event does to this computer's seat. Positions are fractions of the target
/// output.
pub(crate) fn injected(event: InputEvent) -> Option<InjectedEvent> {
    Some(match event {
        InputEvent::PointerMotion { x, y } => InjectedEvent::PointerTo {
            x: f64::from(x),
            y: f64::from(y),
        },
        InputEvent::PointerRelative { dx, dy } => InjectedEvent::PointerMotion {
            dx: f64::from(dx),
            dy: f64::from(dy),
        },
        InputEvent::PointerButton { button, pressed } => InjectedEvent::Button {
            code: button,
            pressed,
        },
        InputEvent::Scroll {
            horizontal_v120,
            vertical_v120,
        } => {
            let (axis, discrete_120) = if vertical_v120 == 0 {
                (ScrollAxis::Horizontal, horizontal_v120)
            } else {
                (ScrollAxis::Vertical, vertical_v120)
            };
            InjectedEvent::Axis {
                axis,
                value: f64::from(discrete_120) / V120_PER_DETENT * SCROLL_UNITS_PER_DETENT,
                discrete_120,
            }
        }
        InputEvent::Key { code, pressed } => InjectedEvent::Key { code, pressed },
        InputEvent::TouchDown { id, x, y } => InjectedEvent::TouchDown {
            id,
            x: f64::from(x),
            y: f64::from(y),
        },
        InputEvent::TouchMotion { id, x, y } => InjectedEvent::TouchMotion {
            id,
            x: f64::from(x),
            y: f64::from(y),
        },
        InputEvent::TouchUp { id } => InjectedEvent::TouchUp { id },
        InputEvent::TouchCancel => InjectedEvent::TouchCancel,
        InputEvent::Frame => return None,
    })
}

/// The keys one side holds down, so a lost key-up can be repaired from a snapshot.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PressedKeys(BTreeSet<u32>);

impl PressedKeys {
    /// Records a key change; `true` when the set changed.
    pub(crate) fn note(&mut self, code: u32, pressed: bool) -> bool {
        if pressed {
            self.0.insert(code)
        } else {
            self.0.remove(&code)
        }
    }

    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    /// The set as a wire snapshot.
    pub(crate) fn snapshot(&self) -> Vec<u8> {
        let mut writer = BatchWriter::default();
        for code in &self.0 {
            writer.push(
                0,
                InputEvent::Key {
                    code: *code,
                    pressed: true,
                },
            );
        }
        writer.take().unwrap_or_default()
    }

    /// Adopts the peer's snapshot and returns the keys this side still held that the peer has
    /// released since.
    pub(crate) fn adopt(&mut self, snapshot: &[u8]) -> Vec<u32> {
        let held: BTreeSet<u32> = messages(snapshot)
            .filter_map(|message| match message.event {
                InputEvent::Key {
                    code,
                    pressed: true,
                } => Some(code),
                _ => None,
            })
            .collect();
        let released = self.0.difference(&held).copied().collect();
        self.0 = held;
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_splits_into_frames_at_frame_events() {
        let mut writer = BatchWriter::default();
        writer.push(5, InputEvent::PointerMotion { x: 0.5, y: 0.25 });
        writer.push(
            5,
            InputEvent::PointerButton {
                button: 0x110,
                pressed: true,
            },
        );
        writer.push(5, InputEvent::Frame);
        writer.push(6, InputEvent::PointerRelative { dx: 3.0, dy: -1.0 });
        let batch = writer.take().unwrap_or_default();
        assert!(writer.take().is_none());
        let frames = input_frames(&batch);
        assert_eq!(frames.len(), 2);
        assert_eq!(
            frames
                .first()
                .map(|frame| (frame.events.len(), frame.time_us)),
            Some((2, 5_000))
        );
        assert_eq!(
            frames.get(1).map(|frame| frame.events.clone()),
            Some(vec![InjectedEvent::PointerMotion { dx: 3.0, dy: -1.0 }])
        );
    }

    #[test]
    fn scrolling_keeps_its_detents() {
        assert_eq!(
            injected(InputEvent::Scroll {
                horizontal_v120: 0,
                vertical_v120: -120,
            }),
            Some(InjectedEvent::Axis {
                axis: ScrollAxis::Vertical,
                value: -15.0,
                discrete_120: -120,
            })
        );
    }

    #[test]
    fn a_snapshot_releases_keys_the_peer_let_go_of() {
        let mut sender = PressedKeys::default();
        assert!(sender.note(30, true));
        assert!(!sender.note(30, true));
        sender.note(42, true);
        let mut receiver = PressedKeys::default();
        receiver.note(30, true);
        receiver.note(42, true);
        receiver.note(57, true);
        assert_eq!(receiver.adopt(&sender.snapshot()), vec![57]);
        sender.clear();
        assert_eq!(receiver.adopt(&sender.snapshot()), vec![30, 42]);
    }
}
