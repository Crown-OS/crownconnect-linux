use std::time::Duration;

use opus::{Application, Bitrate, Channels, Signal};

use crate::media::MediaError;

pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_DURATION: Duration = Duration::from_millis(10);
/// Samples per channel in one 10 ms frame.
pub const FRAME_SAMPLES: usize = 480;
/// The largest packet Opus produces for one frame.
pub const MAX_PACKET_BYTES: usize = 1275;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceChannels {
    Mono,
    Stereo,
}

impl VoiceChannels {
    pub const fn count(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
        }
    }

    const fn opus(self) -> Channels {
        match self {
            Self::Mono => Channels::Mono,
            Self::Stereo => Channels::Stereo,
        }
    }

    pub const fn frame_len(self) -> usize {
        FRAME_SAMPLES * self.count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceConfig {
    pub channels: VoiceChannels,
    pub bitrate_bps: i32,
    /// Discontinuous transmission: send almost nothing while the speaker is silent.
    pub dtx: bool,
    /// Expected packet loss, which sizes the in-band FEC Opus embeds for the previous frame.
    pub expected_loss_percent: u8,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            channels: VoiceChannels::Mono,
            bitrate_bps: 32_000,
            dtx: false,
            expected_loss_percent: 10,
        }
    }
}

/// A 10 ms, 48 kHz Opus voice encoder with in-band FEC always on.
#[derive(Debug)]
pub struct VoiceEncoder {
    encoder: opus::Encoder,
    channels: VoiceChannels,
}

impl VoiceEncoder {
    pub fn new(config: VoiceConfig) -> Result<Self, MediaError> {
        let mut encoder =
            opus::Encoder::new(SAMPLE_RATE, config.channels.opus(), Application::Voip)?;
        encoder.set_bitrate(Bitrate::Bits(config.bitrate_bps))?;
        encoder.set_signal(Signal::Voice)?;
        encoder.set_inband_fec(true)?;
        encoder.set_dtx(config.dtx)?;
        encoder.set_packet_loss_perc(i32::from(config.expected_loss_percent.min(100)))?;
        Ok(Self {
            encoder,
            channels: config.channels,
        })
    }

    /// Encodes one interleaved 10 ms frame into `packet`, returning the packet length.
    pub fn encode(&mut self, frame: &[f32], packet: &mut [u8]) -> Result<usize, MediaError> {
        if frame.len() != self.channels.frame_len() {
            return Err(MediaError::InvalidFrame("voice frames are exactly 10 ms"));
        }
        Ok(self.encoder.encode_float(frame, packet)?)
    }

    /// Feeds the loss rate measured by the transport back into FEC sizing.
    pub fn set_expected_loss(&mut self, percent: u8) -> Result<(), MediaError> {
        Ok(self
            .encoder
            .set_packet_loss_perc(i32::from(percent.min(100)))?)
    }

    pub fn set_bitrate(&mut self, bits_per_second: i32) -> Result<(), MediaError> {
        Ok(self.encoder.set_bitrate(Bitrate::Bits(bits_per_second))?)
    }
}

/// The matching decoder, which rebuilds lost frames from the next packet's FEC or conceals them.
#[derive(Debug)]
pub struct VoiceDecoder {
    decoder: opus::Decoder,
    channels: VoiceChannels,
}

impl VoiceDecoder {
    pub fn new(channels: VoiceChannels) -> Result<Self, MediaError> {
        Ok(Self {
            decoder: opus::Decoder::new(SAMPLE_RATE, channels.opus())?,
            channels,
        })
    }

    /// Decodes a packet into `frame`, returning samples per channel.
    pub fn decode(&mut self, packet: &[u8], frame: &mut [f32]) -> Result<usize, MediaError> {
        Ok(self
            .decoder
            .decode_float(packet, self.frame(frame)?, false)?)
    }

    /// Fills `frame` for a lost packet: recovered from `next` packet's FEC data when it has
    /// already arrived, otherwise synthesised by packet loss concealment.
    pub fn recover(&mut self, next: Option<&[u8]>, frame: &mut [f32]) -> Result<usize, MediaError> {
        let frame = self.frame(frame)?;
        Ok(match next {
            Some(next) if Self::carries_fec(next) => {
                self.decoder.decode_float(next, frame, true)?
            }
            Some(_) => self.decoder.decode_float(&[], frame, false)?,
            None => self.decoder.decode_float(&[], frame, false)?,
        })
    }

    /// Whether `packet` carries in-band FEC for the frame before it.
    pub fn carries_fec(packet: &[u8]) -> bool {
        let Ok(len) = i32::try_from(packet.len()) else {
            return false;
        };
        // SAFETY: packet is a valid slice of len bytes for the duration of the call.
        unsafe { opusic_sys::opus_packet_has_lbrr(packet.as_ptr(), len) == 1 }
    }

    fn frame<'a>(&self, frame: &'a mut [f32]) -> Result<&'a mut [f32], MediaError> {
        frame
            .get_mut(..self.channels.frame_len())
            .ok_or(MediaError::InvalidFrame(
                "output holds less than one 10 ms frame",
            ))
    }
}
