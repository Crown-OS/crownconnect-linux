/// Splits an Annex B byte stream into NAL units, without their start codes.
pub fn nal_units(stream: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = skip_start_code(stream);
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (unit, tail) = match find_start_code(rest) {
            Some((start, code_len)) => (rest.get(..start)?, rest.get(start + code_len..)?),
            None => (rest, &[][..]),
        };
        rest = tail;
        Some(trim_trailing_zeros(unit))
    })
}

/// The HEVC NAL unit type of a unit returned by [`nal_units`].
pub fn hevc_nal_type(unit: &[u8]) -> Option<u8> {
    unit.first().map(|header| (header >> 1) & 0x3f)
}

/// The H.264 NAL unit type of a unit returned by [`nal_units`].
pub fn h264_nal_type(unit: &[u8]) -> Option<u8> {
    unit.first().map(|header| header & 0x1f)
}

fn skip_start_code(stream: &[u8]) -> &[u8] {
    match find_start_code(stream) {
        Some((0, code_len)) => stream.get(code_len..).unwrap_or_default(),
        _ => stream,
    }
}

fn find_start_code(stream: &[u8]) -> Option<(usize, usize)> {
    stream
        .windows(3)
        .position(|window| window == [0, 0, 1])
        .map(|position| {
            if position > 0 && stream.get(position - 1) == Some(&0) {
                (position - 1, 4)
            } else {
                (position, 3)
            }
        })
}

fn trim_trailing_zeros(unit: &[u8]) -> &[u8] {
    let end = unit
        .iter()
        .rposition(|&byte| byte != 0)
        .map_or(0, |last| last + 1);
    unit.get(..end).unwrap_or_default()
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "a test that indexes out of range should fail loudly"
)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let stream = [
            0, 0, 0, 1, 0x40, 0x01, 0, 0, 1, 0x42, 0x01, 0x02, 0, 0, 0, 1, 0x26,
        ];
        let units: Vec<&[u8]> = nal_units(&stream).collect();
        assert_eq!(units, [&[0x40, 0x01][..], &[0x42, 0x01, 0x02], &[0x26]]);
        assert_eq!(hevc_nal_type(units[0]), Some(32));
        assert_eq!(hevc_nal_type(units[2]), Some(19));
    }

    #[test]
    fn empty_stream_has_no_units() {
        assert_eq!(nal_units(&[]).count(), 0);
    }
}
