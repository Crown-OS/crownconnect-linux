use std::io::{self, Read, Write};

use crate::media::video::EncodedUnit;

/// `[len: u32 LE][pts: i64 LE][flags: u8]` before each access unit on the viewer's stdin.
pub const HEADER_BYTES: usize = 13;
/// Access units larger than this are treated as a corrupt stream rather than buffered.
pub const MAX_UNIT_BYTES: usize = 16 << 20;
const KEYFRAME_FLAG: u8 = 1;
const READ_CHUNK: usize = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitHeader {
    pub len: u32,
    pub pts: i64,
    pub keyframe: bool,
}

impl UnitHeader {
    pub fn encode(self) -> [u8; HEADER_BYTES] {
        let mut header = [0; HEADER_BYTES];
        let (len, rest) = header.split_at_mut(4);
        let (pts, flags) = rest.split_at_mut(8);
        len.copy_from_slice(&self.len.to_le_bytes());
        pts.copy_from_slice(&self.pts.to_le_bytes());
        flags.fill(if self.keyframe { KEYFRAME_FLAG } else { 0 });
        header
    }

    pub fn decode(header: &[u8; HEADER_BYTES]) -> Self {
        let (len, rest) = header.split_first_chunk::<4>().unwrap_or((&[0; 4], &[]));
        let (pts, flags) = rest.split_first_chunk::<8>().unwrap_or((&[0; 8], &[]));
        Self {
            len: u32::from_le_bytes(*len),
            pts: i64::from_le_bytes(*pts),
            keyframe: flags
                .first()
                .is_some_and(|flags| flags & KEYFRAME_FLAG != 0),
        }
    }
}

pub fn write_unit(writer: &mut impl Write, unit: &EncodedUnit<'_>) -> io::Result<()> {
    let header = UnitHeader {
        len: u32::try_from(unit.data.len()).map_err(io::Error::other)?,
        pts: unit.pts,
        keyframe: unit.keyframe,
    };
    writer.write_all(&header.encode())?;
    writer.write_all(unit.data)
}

/// Incrementally reassembles framed access units from a (possibly non-blocking) byte stream,
/// reusing one buffer.
#[derive(Debug, Default)]
pub struct UnitReader {
    buffer: Vec<u8>,
    start: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Read(usize),
    WouldBlock,
    Closed,
}

impl UnitReader {
    /// Performs one read from `source` into the buffer.
    pub fn fill_from(&mut self, source: &mut impl Read) -> io::Result<Fill> {
        if self.start > 0 {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        let filled = self.buffer.len();
        self.buffer.resize(filled + READ_CHUNK, 0);
        let result = source.read(self.buffer.get_mut(filled..).unwrap_or_default());
        self.buffer
            .truncate(filled + *result.as_ref().unwrap_or(&0));
        match result {
            Ok(0) => Ok(Fill::Closed),
            Ok(read) => Ok(Fill::Read(read)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(Fill::WouldBlock),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(Fill::WouldBlock),
            Err(error) => Err(error),
        }
    }

    /// The next complete access unit, if one is buffered.
    pub fn next_unit(&mut self) -> io::Result<Option<(UnitHeader, &[u8])>> {
        let pending = self.buffer.get(self.start..).unwrap_or_default();
        let Some(header) = pending
            .first_chunk::<HEADER_BYTES>()
            .map(UnitHeader::decode)
        else {
            return Ok(None);
        };
        let len = header.len as usize;
        if len > MAX_UNIT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "access unit too large",
            ));
        }
        let Some(body) = pending.get(HEADER_BYTES..HEADER_BYTES + len) else {
            return Ok(None);
        };
        self.start += HEADER_BYTES + len;
        Ok(Some((header, body)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let Some((&first, rest)) = self.0.split_first() else {
                return Ok(0);
            };
            let Some(slot) = out.first_mut() else {
                return Ok(0);
            };
            *slot = first;
            self.0 = rest;
            Ok(1)
        }
    }

    #[test]
    fn reassembles_units_arriving_a_byte_at_a_time() -> io::Result<()> {
        let mut stream = Vec::new();
        write_unit(
            &mut stream,
            &EncodedUnit {
                data: &[0, 0, 1, 0x40],
                keyframe: true,
                pts: 5,
            },
        )?;
        write_unit(
            &mut stream,
            &EncodedUnit {
                data: &[0, 0, 1, 0x02, 9],
                keyframe: false,
                pts: 21,
            },
        )?;
        let mut source = Trickle(&stream);
        let mut reader = UnitReader::default();
        let mut units = Vec::new();
        while reader.fill_from(&mut source)? != Fill::Closed {
            while let Some((header, body)) = reader.next_unit()? {
                units.push((header.pts, header.keyframe, body.to_vec()));
            }
        }
        assert_eq!(
            units,
            [
                (5, true, vec![0, 0, 1, 0x40]),
                (21, false, vec![0, 0, 1, 2, 9])
            ]
        );
        Ok(())
    }

    #[test]
    fn rejects_absurd_unit_sizes() {
        let header = UnitHeader {
            len: u32::MAX,
            pts: 0,
            keyframe: false,
        }
        .encode();
        let mut reader = UnitReader::default();
        assert!(reader.fill_from(&mut header.as_slice()).is_ok());
        assert!(reader.next_unit().is_err());
    }
}
