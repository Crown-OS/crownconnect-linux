use crate::ipc::proto::Edge;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrollAxis {
    Vertical,
    Horizontal,
}

/// The capture protocol's edge bits.
const TOP: u32 = 1;
const BOTTOM: u32 = 2;
const LEFT: u32 = 4;
const RIGHT: u32 = 8;

pub(crate) fn edge_bits(edges: &[Edge]) -> u32 {
    edges.iter().fold(0, |bits, edge| {
        bits | match edge {
            Edge::Top => TOP,
            Edge::Bottom => BOTTOM,
            Edge::Left => LEFT,
            Edge::Right => RIGHT,
        }
    })
}

pub(crate) const fn edge_from_bits(bits: u32) -> Option<Edge> {
    match bits {
        TOP => Some(Edge::Top),
        BOTTOM => Some(Edge::Bottom),
        LEFT => Some(Edge::Left),
        RIGHT => Some(Edge::Right),
        _ => None,
    }
}

/// Splits a microsecond timestamp into the protocol's two 32-bit halves.
#[expect(clippy::cast_possible_truncation, reason = "each half fits in 32 bits")]
pub(crate) const fn split_micros(time_us: u64) -> (u32, u32) {
    ((time_us >> 32) as u32, time_us as u32)
}

pub(crate) const fn join_micros(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_round_trip_through_their_bits() {
        assert_eq!(edge_bits(&[Edge::Left, Edge::Right]), LEFT | RIGHT);
        assert_eq!(edge_bits(&[]), 0);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            assert_eq!(edge_from_bits(edge_bits(&[edge])), Some(edge));
        }
        assert_eq!(edge_from_bits(LEFT | TOP), None);
    }

    #[test]
    fn timestamps_split_and_join_losslessly() {
        let (hi, lo) = split_micros(0x0000_0001_0000_0002);
        assert_eq!((hi, lo), (1, 2));
        assert_eq!(join_micros(hi, lo), 0x0000_0001_0000_0002);
    }
}
