use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// Pointer and touch positions are normalised to the mirrored screen: 0.0 is the left or top
/// edge and 1.0 the right or bottom edge, whatever size the viewer window has.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    PointerMotion {
        x: f32,
        y: f32,
    },
    /// Linux evdev button code, such as `BTN_LEFT` (0x110).
    PointerButton {
        button: u32,
        pressed: bool,
    },
    /// Scroll distance in 1/120ths of a wheel detent.
    Scroll {
        horizontal_v120: i32,
        vertical_v120: i32,
    },
    /// Linux evdev key code.
    Key {
        code: u32,
        pressed: bool,
    },
    TouchDown {
        id: i32,
        x: f32,
        y: f32,
    },
    TouchMotion {
        id: i32,
        x: f32,
        y: f32,
    },
    TouchUp {
        id: i32,
    },
    TouchCancel,
    /// Ends a group of events that belong to one moment, like a pointer or touch frame.
    Frame,
    /// Motion in logical pixels rather than to a position, as a shared cursor moves.
    PointerRelative {
        dx: f32,
        dy: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InputMessage {
    /// Milliseconds on the compositor's input clock.
    pub time_ms: u32,
    pub event: InputEvent,
}

/// Postcard bytes of the largest message, well under the one-byte length prefix.
pub const MAX_MESSAGE_BYTES: usize = 32;

/// Writes `[len: u8][postcard message]`.
pub fn write_message(writer: &mut impl Write, message: &InputMessage) -> io::Result<()> {
    let mut frame = [0u8; MAX_MESSAGE_BYTES + 1];
    let (prefix, body) = frame.split_at_mut(1);
    let encoded = postcard::to_slice(message, body)
        .map_err(io::Error::other)?
        .len();
    prefix.fill(u8::try_from(encoded).map_err(io::Error::other)?);
    writer.write_all(frame.get(..=encoded).unwrap_or_default())
}

/// Reads one message written by [`write_message`]; `None` at a clean end of stream.
pub fn read_message(reader: &mut impl Read) -> io::Result<Option<InputMessage>> {
    let mut len = [0u8; 1];
    match reader.read_exact(&mut len) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other?,
    }
    let mut body = [0u8; MAX_MESSAGE_BYTES];
    let body = body
        .get_mut(..usize::from(len[0]))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "input message too long"))?;
    reader.read_exact(body)?;
    postcard::from_bytes(body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_through_a_byte_stream() -> io::Result<()> {
        let messages = [
            InputMessage {
                time_ms: 7,
                event: InputEvent::PointerMotion { x: 0.25, y: 0.75 },
            },
            InputMessage {
                time_ms: 8,
                event: InputEvent::PointerButton {
                    button: 0x110,
                    pressed: true,
                },
            },
            InputMessage {
                time_ms: 9,
                event: InputEvent::Scroll {
                    horizontal_v120: 0,
                    vertical_v120: -120,
                },
            },
            InputMessage {
                time_ms: 10,
                event: InputEvent::TouchDown {
                    id: 3,
                    x: 1.0,
                    y: 0.0,
                },
            },
            InputMessage {
                time_ms: u32::MAX,
                event: InputEvent::Frame,
            },
        ];
        let mut stream = Vec::new();
        for message in &messages {
            write_message(&mut stream, message)?;
        }
        let mut reader = stream.as_slice();
        let mut decoded = Vec::new();
        while let Some(message) = read_message(&mut reader)? {
            decoded.push(message);
        }
        assert_eq!(decoded, messages);
        Ok(())
    }

    #[test]
    fn messages_are_compact() -> io::Result<()> {
        let mut stream = Vec::new();
        let key = InputMessage {
            time_ms: 1000,
            event: InputEvent::Key {
                code: 30,
                pressed: true,
            },
        };
        write_message(&mut stream, &key)?;
        assert!(
            stream.len() <= 8,
            "a key press takes {} bytes",
            stream.len()
        );
        Ok(())
    }
}
