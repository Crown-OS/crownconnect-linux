#![cfg(feature = "media")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "a hardware harness that fails a precondition should fail loudly"
)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crownconnect_linux::media::present::access_unit::write_unit;
use crownconnect_linux::media::video::{
    select_profile, BitDepth, CodecProfile, DownloadBuffer, EncoderBackend, EncoderConfig,
    HardwareCapabilities, Nv12Image, RateControl, VaapiDevice, VideoCodec, VideoDecoder,
    VideoEncoder,
};
use crownconnect_linux::util::annexb::nal_units;
use crownconnect_linux::util::latency::LatencyWindow;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FRAME_RATE: u32 = 60;
const FRAMES: u16 = 120;
const COUNTER_BITS: u32 = 12;
const BLOCK: u32 = 64;
const FORCED_KEYFRAME_AT: u16 = 45;
const BITRATE_DROP_AT: u16 = 80;
const BITRATE_NUDGE_AT: u16 = 100;
const FRAME_PERIOD_US: i64 = 1_000_000 / FRAME_RATE as i64;

struct Hardware {
    device: VaapiDevice,
    capabilities: HardwareCapabilities,
    profile: CodecProfile,
}

fn hardware() -> Option<Hardware> {
    let skip = |reason: &str| {
        eprintln!("skipping VA-API round trip: {reason}");
        None
    };
    if !Path::new("/dev/dri/renderD128").exists() {
        return skip("no /dev/dri/renderD128");
    }
    let device = match VaapiDevice::open_default() {
        Ok(device) => device,
        Err(error) => return skip(&format!("VA-API device unavailable: {error}")),
    };
    let capabilities = match HardwareCapabilities::probe(&device) {
        Ok(capabilities) => capabilities,
        Err(error) => return skip(&format!("probe failed: {error}")),
    };
    let codecs = capabilities.codecs;
    eprintln!(
        "VA-API encode {:?}",
        codecs.encode.iter().collect::<Vec<_>>()
    );
    eprintln!(
        "VA-API decode {:?}",
        codecs.decode.iter().collect::<Vec<_>>()
    );
    match select_profile(codecs.encode, codecs.decode, BitDepth::Eight) {
        Ok(profile) => Some(Hardware {
            device,
            capabilities,
            profile,
        }),
        Err(_) => skip("no HEVC or H.264 hardware encode and decode"),
    }
}

/// A frame whose top rows spell `counter` in big black and white luma blocks.
struct PatternFrame {
    luma: Vec<u8>,
    chroma: Vec<u8>,
}

impl PatternFrame {
    fn new() -> Self {
        Self {
            luma: vec![0; (WIDTH * HEIGHT) as usize],
            chroma: vec![128; (WIDTH * HEIGHT / 2) as usize],
        }
    }

    fn draw(&mut self, counter: u16) -> Nv12Image<'_> {
        let width = WIDTH as usize;
        for (row, line) in self.luma.chunks_mut(width).enumerate() {
            for (column, pixel) in line.iter_mut().enumerate() {
                let bit = (column as u32) / BLOCK;
                *pixel = if (row as u32) < BLOCK && bit < COUNTER_BITS {
                    if counter >> bit & 1 == 1 {
                        235
                    } else {
                        16
                    }
                } else {
                    ((column + row + usize::from(counter) * 4) % 200 + 28) as u8
                };
            }
        }
        Nv12Image {
            width: WIDTH,
            height: HEIGHT,
            luma: &self.luma,
            luma_stride: width,
            chroma: &self.chroma,
            chroma_stride: width,
        }
    }
}

fn read_counter(image: &Nv12Image<'_>) -> u16 {
    let centre_row = image
        .luma_rows()
        .nth((BLOCK / 2) as usize)
        .expect("frame has a counter row");
    (0..COUNTER_BITS).fold(0, |counter, bit| {
        let centre = (bit * BLOCK + BLOCK / 2) as usize;
        let block_luma = centre_row[centre - 8..centre + 8]
            .iter()
            .map(|&y| u32::from(y))
            .sum::<u32>()
            / 16;
        if block_luma > 128 {
            counter | 1 << bit
        } else {
            counter
        }
    })
}

fn report(label: &str, window: &LatencyWindow) {
    let summary = window.summary().expect("latency samples were recorded");
    eprintln!(
        "{label}: median {:?}, p99 {:?} over {} frames",
        summary.median, summary.p99, summary.samples
    );
}

fn ffprobe_frame_count(stream: &Path, codec: VideoCodec) -> Option<usize> {
    let format = match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::Hevc => "hevc",
        VideoCodec::Av1 => return None,
    };
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-f",
            format,
            "-count_frames",
            "-select_streams",
            "v:0",
        ])
        .args([
            "-show_entries",
            "stream=nb_read_frames,codec_name,width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(stream)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    eprintln!("ffprobe: {}", text.trim());
    text.trim().rsplit(',').next()?.parse().ok()
}

#[test]
fn encode_decode_round_trip_preserves_every_frame_in_order() {
    let Some(hardware) = hardware() else { return };
    let codecs = hardware.capabilities.codecs;
    let profiles = [
        CodecProfile::HevcMain,
        CodecProfile::H264High,
        CodecProfile::H264ConstrainedBaseline,
    ]
    .into_iter()
    .filter(|&profile| codecs.encode.contains(profile) && codecs.decode.contains(profile));
    for profile in profiles {
        round_trip(&hardware, profile);
    }
}

fn round_trip(hardware: &Hardware, profile: CodecProfile) {
    let config = EncoderConfig {
        profile,
        width: WIDTH,
        height: HEIGHT,
        frame_rate: FRAME_RATE,
        bitrate_bps: 12_000_000,
        rate_control: RateControl::Cbr,
        slices: 2,
        backend: EncoderBackend::Vaapi,
    };
    eprintln!(
        "round trip with {profile:?}, {:?}",
        hardware.capabilities.encode_features(profile)
    );
    let mut encoder =
        VideoEncoder::new(&hardware.device, &hardware.capabilities, config).expect("encoder opens");
    let codec = profile.codec();
    let mut decoder = VideoDecoder::new(&hardware.device, codec).expect("decoder opens");
    let mut download = DownloadBuffer::new().expect("download buffer");
    let mut pattern = PatternFrame::new();
    let mut stream = Vec::new();
    let mut framed = Vec::new();
    let mut encode_latency = LatencyWindow::with_capacity(FRAMES.into());
    let mut round_trip_latency = LatencyWindow::with_capacity(FRAMES.into());
    let mut readback_latency = LatencyWindow::with_capacity(FRAMES.into());
    let mut decoded = Vec::with_capacity(FRAMES.into());

    for counter in 0..FRAMES {
        if counter == FORCED_KEYFRAME_AT {
            encoder.request_keyframe();
        }
        if counter == BITRATE_DROP_AT {
            encoder.set_target_bitrate(4_000_000);
        }
        if counter == BITRATE_NUDGE_AT {
            encoder.set_target_bitrate(3_000_000);
        }
        let pts = i64::from(counter) * FRAME_PERIOD_US;
        let image = pattern.draw(counter);
        let started = Instant::now();
        encoder.encode_nv12(&image, pts).expect("frame encodes");
        let unit = encoder
            .next_unit()
            .expect("encoder output")
            .expect("one unit per frame");
        encode_latency.record(started.elapsed());
        assert_eq!(unit.pts, pts, "units come out in capture order");
        assert!(nal_units(unit.data).count() >= 1);
        if counter == 0 || counter == FORCED_KEYFRAME_AT {
            assert!(unit.keyframe, "frame {counter} should be an IDR");
        }
        stream.extend_from_slice(unit.data);
        write_unit(&mut framed, &unit).expect("unit is framed");
        decoder.decode(unit.data, unit.pts).expect("unit decodes");
        let frame = decoder
            .next_frame()
            .expect("decoder output")
            .expect("low-delay output");
        round_trip_latency.record(started.elapsed());
        let image = frame.download(&mut download).expect("frame downloads");
        readback_latency.record(started.elapsed());
        decoded.push((frame.pts(), read_counter(&image)));
    }

    let expected: Vec<(i64, u16)> = (0..FRAMES)
        .map(|counter| (i64::from(counter) * FRAME_PERIOD_US, counter))
        .collect();
    assert_eq!(
        decoded, expected,
        "every frame decodes once, in order, with its counter"
    );
    report("encode (upload + VA-API encode)", &encode_latency);
    report("encode + decode (frame returned)", &round_trip_latency);
    report("encode + decode + readback (GPU synced)", &readback_latency);

    let path =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("round_trip_{profile:?}.bitstream"));
    std::fs::write(&path, &stream).expect("elementary stream is written");
    std::fs::write(path.with_extension("units"), &framed).expect("framed stream is written");
    eprintln!("wrote {} bytes to {}", stream.len(), path.display());
    match ffprobe_frame_count(&path, codec) {
        Some(count) => assert_eq!(count, usize::from(FRAMES)),
        None => eprintln!("ffprobe unavailable; skipped stream validation"),
    }
}

#[test]
fn decoded_surfaces_re_encode_zero_copy_through_dmabuf() {
    let Some(hardware) = hardware() else { return };
    let config = EncoderConfig {
        profile: hardware.profile,
        width: WIDTH,
        height: HEIGHT,
        frame_rate: FRAME_RATE,
        bitrate_bps: 12_000_000,
        rate_control: RateControl::Vbr,
        slices: 1,
        backend: EncoderBackend::Vaapi,
    };
    let codec = hardware.profile.codec();
    let mut source = VideoEncoder::new(&hardware.device, &hardware.capabilities, config)
        .expect("source encoder");
    let mut decoder = VideoDecoder::new(&hardware.device, codec).expect("decoder");
    let mut transcoder = VideoEncoder::new(&hardware.device, &hardware.capabilities, config)
        .expect("dmabuf encoder");
    let mut verifier = VideoDecoder::new(&hardware.device, codec).expect("verifying decoder");
    let mut download = DownloadBuffer::new().expect("download buffer");
    let mut pattern = PatternFrame::new();
    let mut surface_slots: Vec<u32> = Vec::new();
    let mut dmabuf_latency = LatencyWindow::with_capacity(60);

    for counter in 0..60u16 {
        let pts = i64::from(counter) * FRAME_PERIOD_US;
        source
            .encode_nv12(&pattern.draw(counter), pts)
            .expect("source encodes");
        let unit = source
            .next_unit()
            .expect("source output")
            .expect("source unit");
        decoder.decode(unit.data, unit.pts).expect("source decodes");
        let frame = decoder
            .next_frame()
            .expect("decoder output")
            .expect("decoded frame");

        let surface = frame.surface_id();
        let slot = surface_slots
            .iter()
            .position(|&id| id == surface)
            .unwrap_or_else(|| {
                surface_slots.push(surface);
                surface_slots.len() - 1
            })
            % 4;
        let export = frame.export_dmabuf().expect("surface exports");
        let dmabuf = export.frame().expect("export is NV12");
        let started = Instant::now();
        transcoder
            .encode_dmabuf(slot, &dmabuf, pts)
            .expect("dmabuf encodes");
        let unit = transcoder
            .next_unit()
            .expect("dmabuf output")
            .expect("dmabuf unit");
        dmabuf_latency.record(started.elapsed());
        verifier
            .decode(unit.data, unit.pts)
            .expect("re-encoded unit decodes");
        let verified = verifier
            .next_frame()
            .expect("verifier output")
            .expect("verified frame");
        let image = verified.download(&mut download).expect("download");
        assert_eq!(
            read_counter(&image),
            counter,
            "dmabuf path keeps frame {counter}"
        );
    }
    eprintln!("decoder pool surfaces seen: {}", surface_slots.len());
    report("dmabuf import + encode", &dmabuf_latency);
    assert!(dmabuf_latency
        .summary()
        .is_some_and(|s| s.p99 < Duration::from_millis(50)));
}
