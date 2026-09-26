//! One pointer and keyboard across computers. Outgoing, this computer's input is captured once
//! the cursor crosses the armed edge and streamed to the peer until the peer hands it back.
//! Incoming, a peer's cursor enters at the facing edge, moves by the peer's relative motion,
//! and is handed back when it crosses that edge again.

use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use llts_signaling::message::{Edge as WireEdge, EdgePosition, UnicursorEnter, UnicursorLeave};

use super::input_wire::{BatchWriter, PressedKeys};
use super::pipeline::{Control, PipelineContext, Stopped};
use super::platform::MediaPlatform;
use super::FeatureError;
use crate::ipc::proto::Edge;
use crate::media::present::input::InputEvent;
use crate::peer::convert::signaling_edge;
use crate::peer::UnicursorSignal;
use crate::util::numeric::{narrow_f32, saturating_i32};
use crate::wayland::input_capture::CaptureEvent;
use crate::wayland::{channel_outlet, ScrollAxis, WaylandError};

/// Pointer motion arrives at the mouse's report rate, so the queue absorbs a few frames' worth.
const CAPTURE_EVENT_DEPTH: usize = 1024;
const CAPTURE_WAIT: Duration = Duration::from_millis(5);
/// How far inside the edge the pointer comes back, so it does not cross straight out again.
const RETURN_INSET: f64 = 0.005;

/// The edge of the other screen that faces `edge` of this one.
pub(crate) const fn facing(edge: WireEdge) -> WireEdge {
    match edge {
        WireEdge::Left => WireEdge::Right,
        WireEdge::Right => WireEdge::Left,
        WireEdge::Top => WireEdge::Bottom,
        WireEdge::Bottom => WireEdge::Top,
    }
}

pub(crate) fn edge_position(fraction: f64) -> EdgePosition {
    let scaled = saturating_i32((fraction.clamp(0.0, 1.0) * f64::from(u16::MAX)).round());
    EdgePosition(u16::try_from(scaled).unwrap_or(u16::MAX))
}

pub(crate) fn edge_fraction(position: EdgePosition) -> f64 {
    f64::from(position.0) / f64::from(u16::MAX)
}

/// The point `along` an edge, as fractions of the screen, `inset` inside it.
pub(crate) fn point_on_edge(edge: WireEdge, along: f64, inset: f64) -> (f64, f64) {
    match edge {
        WireEdge::Left => (inset, along),
        WireEdge::Right => (1.0 - inset, along),
        WireEdge::Top => (along, inset),
        WireEdge::Bottom => (along, 1.0 - inset),
    }
}

/// Captures this computer's input at `edge` and streams it to the peer while the cursor is
/// over there, until stopped.
pub(crate) fn run_capture(
    context: &PipelineContext,
    platform: &dyn MediaPlatform,
    edge: Edge,
) -> Result<(), FeatureError> {
    let (sender, events) = sync_channel(CAPTURE_EVENT_DEPTH);
    let capture = platform.input_capture(channel_outlet(sender))?;
    capture.arm(&[edge])?;
    let mut stream = CapturedInput::default();
    let started = Instant::now();
    loop {
        loop {
            match context.pending_control() {
                Ok(Some(Control::UnicursorLeave(leave))) if stream.captured => {
                    let along = edge_fraction(leave.position);
                    let (x, y) = point_on_edge(signaling_edge(edge), along, RETURN_INSET);
                    capture.release(None, x, y)?;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(Stopped) => {
                    let _ = capture.arm(&[]);
                    return Ok(());
                }
            }
        }
        let event = match events.recv_timeout(CAPTURE_WAIT) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err(WaylandError::ThreadGone.into()),
        };
        let time_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        stream.on_event(context, time_ms, event);
    }
}

/// The captured input on its way to the peer.
#[derive(Debug, Default)]
struct CapturedInput {
    captured: bool,
    batch: BatchWriter,
    keys: PressedKeys,
}

impl CapturedInput {
    fn on_event(&mut self, context: &PipelineContext, time_ms: u32, event: CaptureEvent) {
        let event = match event {
            CaptureEvent::Entered { edge, position } => {
                self.captured = true;
                context.send_unicursor(UnicursorSignal::Enter(UnicursorEnter {
                    edge: signaling_edge(edge),
                    position: edge_position(position),
                }));
                return;
            }
            CaptureEvent::Released => {
                self.captured = false;
                self.flush(context);
                self.keys.clear();
                context.send_key_state(self.keys.snapshot());
                return;
            }
            _ if !self.captured => return,
            CaptureEvent::Motion { dx, dy } => InputEvent::PointerRelative {
                dx: narrow_f32(dx),
                dy: narrow_f32(dy),
            },
            CaptureEvent::Button { code, pressed } => InputEvent::PointerButton {
                button: code,
                pressed,
            },
            CaptureEvent::Axis {
                axis, discrete_120, ..
            } => match axis {
                ScrollAxis::Vertical => InputEvent::Scroll {
                    horizontal_v120: 0,
                    vertical_v120: discrete_120,
                },
                ScrollAxis::Horizontal => InputEvent::Scroll {
                    horizontal_v120: discrete_120,
                    vertical_v120: 0,
                },
            },
            CaptureEvent::Key { code, pressed } => {
                self.batch.push(time_ms, InputEvent::Key { code, pressed });
                self.flush(context);
                if self.keys.note(code, pressed) {
                    context.send_key_state(self.keys.snapshot());
                }
                return;
            }
            CaptureEvent::Modifiers { .. } => return,
            CaptureEvent::Frame { .. } => InputEvent::Frame,
        };
        let ends_frame = event == InputEvent::Frame;
        self.batch.push(time_ms, event);
        if ends_frame {
            self.flush(context);
        }
    }

    fn flush(&mut self, context: &PipelineContext) {
        if let Some(events) = self.batch.take() {
            context.send_input(events);
        }
    }
}

/// A peer's cursor on this screen, in logical pixels of the output it moves on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RemoteCursor {
    /// This screen's edge that faces the peer, where the cursor came in and goes back out.
    entry: WireEdge,
    /// Where it entered, as fractions of the screen, until the screen's size is known.
    entered_at: (f64, f64),
    position: Option<(f64, f64)>,
}

impl RemoteCursor {
    /// The cursor that crossed the peer's `edge` at `position`, and the point it appears at.
    pub(crate) fn entering(enter: UnicursorEnter) -> Self {
        let entry = facing(enter.edge);
        Self {
            entry,
            entered_at: point_on_edge(entry, edge_fraction(enter.position), RETURN_INSET),
            position: None,
        }
    }

    pub(crate) const fn entry_point(&self) -> (f64, f64) {
        self.entered_at
    }

    /// Follows relative motion on a screen of `size`; the handover once it crossed back.
    pub(crate) fn moved(
        &mut self,
        (dx, dy): (f64, f64),
        size: Option<(f64, f64)>,
    ) -> Option<UnicursorLeave> {
        let (width, height) = size?;
        let (x, y) = self
            .position
            .unwrap_or((self.entered_at.0 * width, self.entered_at.1 * height));
        let (x, y) = (x + dx, y + dy);
        let crossed = match self.entry {
            WireEdge::Left => (x < 0.0).then_some(y / height),
            WireEdge::Right => (x > width).then_some(y / height),
            WireEdge::Top => (y < 0.0).then_some(x / width),
            WireEdge::Bottom => (y > height).then_some(x / width),
        };
        if let Some(along) = crossed {
            return Some(UnicursorLeave {
                edge: self.entry,
                position: edge_position(along),
            });
        }
        self.position = Some((x.clamp(0.0, width), y.clamp(0.0, height)));
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_positions_span_the_whole_edge() {
        assert_eq!(edge_position(0.0), EdgePosition(0));
        assert_eq!(edge_position(1.5), EdgePosition(u16::MAX));
        assert!((edge_fraction(edge_position(0.25)) - 0.25).abs() < 1e-4);
    }

    #[test]
    fn a_remote_cursor_goes_back_across_the_edge_it_came_in_by() {
        let mut cursor = RemoteCursor::entering(UnicursorEnter {
            edge: WireEdge::Right,
            position: edge_position(0.5),
        });
        let (x, y) = cursor.entry_point();
        assert!(x < 0.01 && (y - 0.5).abs() < 1e-3);
        let screen = Some((1000.0, 500.0));
        assert_eq!(cursor.moved((10.0, 0.0), None), None);
        assert_eq!(cursor.moved((200.0, 50.0), screen), None);
        assert_eq!(cursor.moved((-100.0, 0.0), screen), None);
        let left = cursor.moved((-150.0, 0.0), screen);
        assert_eq!(
            left.map(|left| left.edge),
            Some(WireEdge::Left),
            "it leaves across this screen's left edge, back to the peer on the left"
        );
        assert!(left.is_some_and(|left| left.position.0 > u16::MAX / 2));
    }
}
