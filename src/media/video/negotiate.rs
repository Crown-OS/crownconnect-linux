use serde::{Deserialize, Serialize};

use super::codec::{BitDepth, CodecProfile, ProfileSet};
use crate::media::MediaError;

/// What a device's video hardware can do, as exchanged with peers during stream setup.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodecCapabilities {
    pub encode: ProfileSet,
    pub decode: ProfileSet,
}

const PREFERRED_8_BIT: [CodecProfile; 3] = [
    CodecProfile::HevcMain,
    CodecProfile::H264High,
    CodecProfile::H264ConstrainedBaseline,
];

/// Picks the best profile this device can encode and the peer can decode: HEVC first, then
/// H.264. A 10-bit request prefers HEVC Main 10 and falls back to 8-bit profiles. AV1 is probed
/// but not selected yet.
pub fn select_profile(
    local_encode: ProfileSet,
    peer_decode: ProfileSet,
    depth: BitDepth,
) -> Result<CodecProfile, MediaError> {
    let common = local_encode.intersection(peer_decode);
    let ten_bit = match depth {
        BitDepth::Ten => Some(CodecProfile::HevcMain10),
        BitDepth::Eight => None,
    };
    ten_bit
        .into_iter()
        .chain(PREFERRED_8_BIT)
        .find(|&profile| common.contains(profile))
        .ok_or(MediaError::NoCommonCodec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use CodecProfile::*;

    fn set(profiles: &[CodecProfile]) -> ProfileSet {
        profiles.iter().copied().collect()
    }

    #[test]
    fn prefers_hevc_when_both_sides_support_it() {
        let both = set(&[HevcMain, H264High]);
        assert_eq!(
            select_profile(both, both, BitDepth::Eight).ok(),
            Some(HevcMain)
        );
    }

    #[test]
    fn falls_back_to_h264_when_peer_lacks_hevc() {
        let local = set(&[HevcMain, HevcMain10, H264High, H264ConstrainedBaseline]);
        let peer = set(&[H264ConstrainedBaseline, Av1Main]);
        let chosen = select_profile(local, peer, BitDepth::Eight).ok();
        assert_eq!(chosen, Some(H264ConstrainedBaseline));
    }

    #[test]
    fn ten_bit_prefers_main10_then_falls_back_to_8_bit() {
        let local = set(&[HevcMain, HevcMain10]);
        let chosen = select_profile(local, local, BitDepth::Ten).ok();
        assert_eq!(chosen, Some(HevcMain10));
        let chosen = select_profile(local, set(&[HevcMain]), BitDepth::Ten).ok();
        assert_eq!(chosen, Some(HevcMain));
    }

    #[test]
    fn eight_bit_never_picks_main10() {
        let only_ten = set(&[HevcMain10]);
        let result = select_profile(only_ten, only_ten, BitDepth::Eight);
        assert!(matches!(result, Err(MediaError::NoCommonCodec)));
    }

    #[test]
    fn av1_is_not_selected_yet() {
        let av1 = set(&[Av1Main]);
        assert!(select_profile(av1, av1, BitDepth::Eight).is_err());
    }

    #[test]
    fn capabilities_are_two_bytes_on_the_wire() -> Result<(), postcard::Error> {
        let caps = CodecCapabilities {
            encode: set(&[HevcMain, H264High]),
            decode: set(&CodecProfile::ALL),
        };
        let bytes = postcard::to_allocvec(&caps)?;
        assert_eq!(bytes.len(), 2);
        assert_eq!(postcard::from_bytes::<CodecCapabilities>(&bytes)?, caps);
        Ok(())
    }
}
