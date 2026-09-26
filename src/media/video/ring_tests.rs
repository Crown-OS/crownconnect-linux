#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a hardware test that fails a precondition should fail loudly"
)]

use std::path::Path;

use ffmpeg_next::ffi::{
    av_frame_get_buffer, av_hwframe_get_buffer, av_hwframe_transfer_data, AVPixelFormat,
};

use super::hw::{new_frames_context, AvFrame, FramesSpec};
use super::map::ExportedDmabuf;
use super::slot_cache::MAX_RING_SLOTS;
use super::{
    select_profile, BitDepth, DownloadBuffer, DrmFourcc, EncoderBackend, EncoderConfig,
    HardwareCapabilities, RateControl, VaapiDevice, VideoDecoder, VideoEncoder,
};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 384;
const ROUNDS: usize = 3;
const LUMA_TOLERANCE: i32 = 10;

fn hardware() -> Option<(VaapiDevice, HardwareCapabilities)> {
    if !Path::new(super::hw::DEFAULT_RENDER_NODE).exists() {
        eprintln!("skipping: no render node");
        return None;
    }
    let device = VaapiDevice::open_default().ok()?;
    let capabilities = HardwareCapabilities::probe(&device).ok()?;
    Some((device, capabilities))
}

/// Surfaces filled with one flat level each, as a compositor's capture ring would hand out.
fn capture_ring(
    device: &VaapiDevice,
    fourcc: DrmFourcc,
    levels: &[u8],
) -> Vec<(AvFrame, ExportedDmabuf)> {
    let software_format = match fourcc {
        DrmFourcc::Nv12 => AVPixelFormat::AV_PIX_FMT_NV12,
        DrmFourcc::Xrgb8888 => AVPixelFormat::AV_PIX_FMT_BGR0,
    };
    let spec = FramesSpec {
        software_format,
        width: WIDTH,
        height: HEIGHT,
        pool_size: 0,
    };
    let frames = new_frames_context(device, spec).expect("capture frames context");
    levels
        .iter()
        .map(|&level| {
            let mut pixels = AvFrame::new().expect("frame");
            let raw = pixels.get_mut();
            raw.format = software_format as i32;
            raw.width = WIDTH as i32;
            raw.height = HEIGHT as i32;
            // SAFETY: format and size are set on an empty frame.
            assert!(unsafe { av_frame_get_buffer(pixels.as_mut_ptr(), 0) } >= 0);
            fill(&mut pixels, fourcc, level);
            let mut surface = AvFrame::new().expect("surface");
            // SAFETY: frames is initialised; surface is empty; pixels matches its sw_format.
            unsafe {
                assert!(av_hwframe_get_buffer(frames.as_ptr(), surface.as_mut_ptr(), 0) >= 0);
                assert!(av_hwframe_transfer_data(surface.as_mut_ptr(), pixels.as_ptr(), 0) >= 0);
            }
            let export = ExportedDmabuf::export(&surface).expect("surface exports");
            (surface, export)
        })
        .collect()
}

fn fill(frame: &mut AvFrame, fourcc: DrmFourcc, level: u8) {
    let raw = frame.get();
    let plane = |index: usize, rows: u32| {
        let len = raw.linesize[index] as usize * rows as usize;
        // SAFETY: av_frame_get_buffer allocated linesize * rows bytes for the plane.
        unsafe { std::slice::from_raw_parts_mut(raw.data[index], len) }
    };
    match fourcc {
        DrmFourcc::Nv12 => {
            plane(0, HEIGHT).fill(level);
            plane(1, HEIGHT / 2).fill(128);
        }
        DrmFourcc::Xrgb8888 => plane(0, HEIGHT).fill(level),
    }
}

fn centre_luma(decoded: &super::DecodedFrame, download: &mut DownloadBuffer) -> i32 {
    let image = decoded.download(download).expect("download");
    let row = image
        .luma_rows()
        .nth(HEIGHT as usize / 2)
        .expect("centre row");
    let centre = WIDTH as usize / 2;
    row[centre - 16..centre + 16]
        .iter()
        .map(|&y| i32::from(y))
        .sum::<i32>()
        / 32
}

fn encode_ring(
    fourcc: DrmFourcc,
    levels: [u8; MAX_RING_SLOTS],
    expected_luma: [i32; MAX_RING_SLOTS],
) {
    let Some((device, capabilities)) = hardware() else {
        return;
    };
    let codecs = capabilities.codecs;
    let Ok(profile) = select_profile(codecs.encode, codecs.decode, BitDepth::Eight) else {
        eprintln!("skipping: no common hardware codec");
        return;
    };
    let ring = capture_ring(&device, fourcc, &levels);
    let config = EncoderConfig {
        profile,
        width: WIDTH,
        height: HEIGHT,
        frame_rate: 60,
        bitrate_bps: 8_000_000,
        rate_control: RateControl::Cbr,
        slices: 1,
        backend: EncoderBackend::Vaapi,
    };
    let mut encoder = VideoEncoder::new(&device, &capabilities, config).expect("encoder");
    let mut decoder = VideoDecoder::new(&device, profile.codec()).expect("decoder");
    let mut download = DownloadBuffer::new().expect("download buffer");
    for index in 0..ROUNDS * MAX_RING_SLOTS {
        let slot = index % MAX_RING_SLOTS;
        let dmabuf = ring[slot].1.frame().expect("dmabuf frame");
        assert_eq!(dmabuf.fourcc, fourcc);
        encoder
            .encode_dmabuf(slot, &dmabuf, index as i64)
            .expect("dmabuf encodes");
        let unit = encoder.next_unit().expect("output").expect("unit");
        decoder.decode(unit.data, unit.pts).expect("decodes");
        let decoded = decoder
            .next_frame()
            .expect("decoder output")
            .expect("frame");
        let luma = centre_luma(&decoded, &mut download);
        assert!(
            (luma - expected_luma[slot]).abs() <= LUMA_TOLERANCE,
            "{fourcc:?} slot {slot}: luma {luma}, expected {}",
            expected_luma[slot]
        );
    }
    assert_eq!(
        encoder.dmabuf_imports(),
        MAX_RING_SLOTS as u64,
        "each ring slot is imported once"
    );
}

#[test]
fn nv12_ring_slots_are_imported_once_and_encode_their_content() {
    encode_ring(DrmFourcc::Nv12, [30, 90, 150, 210], [30, 90, 150, 210]);
}

#[test]
fn xrgb_ring_is_converted_to_nv12_on_the_gpu() {
    encode_ring(DrmFourcc::Xrgb8888, [0, 85, 170, 255], [16, 89, 162, 235]);
}
