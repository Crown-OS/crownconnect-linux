use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BitDepth {
    Eight,
    Ten,
}

/// A codec profile a hardware block can encode or decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum CodecProfile {
    H264ConstrainedBaseline,
    H264High,
    HevcMain,
    HevcMain10,
    Av1Main,
}

impl CodecProfile {
    pub const ALL: [Self; 5] = [
        Self::H264ConstrainedBaseline,
        Self::H264High,
        Self::HevcMain,
        Self::HevcMain10,
        Self::Av1Main,
    ];

    pub const fn codec(self) -> VideoCodec {
        match self {
            Self::H264ConstrainedBaseline | Self::H264High => VideoCodec::H264,
            Self::HevcMain | Self::HevcMain10 => VideoCodec::Hevc,
            Self::Av1Main => VideoCodec::Av1,
        }
    }

    pub const fn bit_depth(self) -> BitDepth {
        match self {
            Self::HevcMain10 => BitDepth::Ten,
            _ => BitDepth::Eight,
        }
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A compact set of [`CodecProfile`]s, one byte on the wire.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileSet(u8);

impl ProfileSet {
    pub const EMPTY: Self = Self(0);

    #[must_use]
    pub const fn with(self, profile: CodecProfile) -> Self {
        Self(self.0 | profile.bit())
    }

    pub const fn contains(self, profile: CodecProfile) -> bool {
        self.0 & profile.bit() != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub fn iter(self) -> impl Iterator<Item = CodecProfile> {
        CodecProfile::ALL
            .into_iter()
            .filter(move |&profile| self.contains(profile))
    }
}

impl FromIterator<CodecProfile> for ProfileSet {
    fn from_iter<I: IntoIterator<Item = CodecProfile>>(profiles: I) -> Self {
        profiles.into_iter().fold(Self::EMPTY, Self::with)
    }
}

/// The encoder implementation a stream runs on. NVENC joins VA-API once NVIDIA hardware is
/// supported.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncoderBackend {
    #[default]
    Vaapi,
}

impl EncoderBackend {
    pub const fn encoder_name(self, codec: VideoCodec) -> &'static str {
        match (self, codec) {
            (Self::Vaapi, VideoCodec::H264) => "h264_vaapi",
            (Self::Vaapi, VideoCodec::Hevc) => "hevc_vaapi",
            (Self::Vaapi, VideoCodec::Av1) => "av1_vaapi",
        }
    }
}
