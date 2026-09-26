#![cfg(feature = "media")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "a test that fails a precondition should fail loudly"
)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crownconnect_linux::media::audio::opus::{
    VoiceChannels, FRAME_SAMPLES, MAX_PACKET_BYTES, SAMPLE_RATE,
};
use crownconnect_linux::media::audio::{
    pcm_ring, VirtualMicrophone, VoiceConfig, VoiceDecoder, VoiceEncoder,
};

const FRAMES: usize = 300;
const LOSS_PERCENT: u64 = 10;

fn speech_like(sample: usize) -> f32 {
    let time = sample as f32 / SAMPLE_RATE as f32;
    let pitch = 140.0 + 30.0 * (std::f32::consts::TAU * 3.0 * time).sin();
    let envelope = 0.5 + 0.5 * (std::f32::consts::TAU * 4.0 * time).sin().abs();
    let voiced: f32 = (1..=8u8)
        .map(|harmonic| {
            let harmonic = f32::from(harmonic);
            (std::f32::consts::TAU * pitch * harmonic * time).sin() / harmonic
        })
        .sum();
    0.25 * envelope * voiced
}

const fn hashed_loss(frame: usize) -> bool {
    ((frame as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) % 100 < LOSS_PERCENT
}

/// Deterministic ~10% loss pattern that never drops two packets in a row, as FEC can only rebuild
/// a single lost packet from its successor.
const fn lost(frame: usize) -> bool {
    frame > 0 && hashed_loss(frame) && !hashed_loss(frame - 1)
}

/// Mean absolute per-frame energy error, in dB, of `decoded` against `clean` over `frames`.
fn energy_error_db(clean: &[f32], decoded: &[f32], frames: &[usize]) -> f32 {
    let energy = |signal: &[f32], frame: usize| {
        let samples = &signal[frame * FRAME_SAMPLES..(frame + 1) * FRAME_SAMPLES];
        samples
            .iter()
            .map(|sample| sample * sample)
            .sum::<f32>()
            .max(1e-9)
    };
    let total: f32 = frames
        .iter()
        .map(|&frame| (10.0 * (energy(decoded, frame) / energy(clean, frame)).log10()).abs())
        .sum();
    total / frames.len() as f32
}

fn decode_with_loss(packets: &[Vec<u8>], use_fec: bool) -> Vec<f32> {
    let mut decoder = VoiceDecoder::new(VoiceChannels::Mono).expect("decoder");
    let mut output = vec![0.0; FRAMES * FRAME_SAMPLES];
    for (index, frame) in output.chunks_mut(FRAME_SAMPLES).enumerate() {
        let decoded = if lost(index) {
            let next = packets
                .get(index + 1)
                .filter(|_| use_fec)
                .map(Vec::as_slice);
            decoder
                .recover(next, frame)
                .expect("loss is recovered or concealed")
        } else {
            decoder
                .decode(&packets[index], frame)
                .expect("packet decodes")
        };
        assert_eq!(decoded, FRAME_SAMPLES);
    }
    output
}

#[test]
fn fec_recovers_ten_percent_loss_better_than_concealment() {
    let reference: Vec<f32> = (0..FRAMES * FRAME_SAMPLES).map(speech_like).collect();
    let mut encoder = VoiceEncoder::new(VoiceConfig {
        expected_loss_percent: 10,
        ..VoiceConfig::default()
    })
    .expect("encoder");
    let mut packet = [0u8; MAX_PACKET_BYTES];
    let started = Instant::now();
    let packets: Vec<Vec<u8>> = reference
        .chunks(FRAME_SAMPLES)
        .map(|frame| {
            let len = encoder.encode(frame, &mut packet).expect("frame encodes");
            packet[..len].to_vec()
        })
        .collect();
    let per_frame = started.elapsed() / FRAMES as u32;
    let fec_packets = packets
        .iter()
        .filter(|packet| VoiceDecoder::carries_fec(packet))
        .count();
    let lost_frames: Vec<usize> = (0..FRAMES).filter(|&frame| lost(frame)).collect();
    let loss = lost_frames.len() as f32 / FRAMES as f32 * 100.0;
    let average_bytes = packets.iter().map(Vec::len).sum::<usize>() / FRAMES;

    let with_fec = decode_with_loss(&packets, true);
    let concealed = decode_with_loss(&packets, false);
    let clean = {
        let mut decoder = VoiceDecoder::new(VoiceChannels::Mono).expect("decoder");
        let mut output = vec![0.0; FRAMES * FRAME_SAMPLES];
        for (frame, packet) in output.chunks_mut(FRAME_SAMPLES).zip(&packets) {
            decoder.decode(packet, frame).expect("packet decodes");
        }
        output
    };
    let fec_error = energy_error_db(&clean, &with_fec, &lost_frames);
    let plc_error = energy_error_db(&clean, &concealed, &lost_frames);
    eprintln!(
        "opus: {loss:.1}% loss over {FRAMES} frames, {average_bytes} B/packet, {fec_packets} carry FEC, \
         encode {per_frame:?}/frame; lost-frame energy error vs clean decode: FEC {fec_error:.2} dB, \
         PLC {plc_error:.2} dB"
    );
    assert!((8.0..=12.0).contains(&loss), "loss pattern is about 10%");
    assert!(
        fec_packets * 10 >= FRAMES * 9,
        "nearly every packet carries FEC"
    );
    assert!(fec_error < plc_error, "FEC must beat plain concealment");
}

#[test]
fn rejects_frames_that_are_not_ten_milliseconds() {
    let mut encoder = VoiceEncoder::new(VoiceConfig::default()).expect("encoder");
    let mut packet = [0u8; MAX_PACKET_BYTES];
    assert!(encoder.encode(&[0.0; 960], &mut packet).is_err());
    assert!(encoder.set_expected_loss(25).is_ok());
}

fn pipewire_running() -> bool {
    std::env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|dir| Path::new(&dir).join("pipewire-0").exists())
}

fn nodes_listing() -> Option<String> {
    let output = Command::new("pw-cli").args(["ls", "Node"]).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn virtual_microphone_appears_and_disappears_in_pipewire() {
    if !pipewire_running() || nodes_listing().is_none() {
        eprintln!("skipping virtual microphone test: no PipeWire daemon or pw-cli");
        return;
    }
    let device = format!("Test Phone {}", std::process::id());
    let description = format!("CrownConnect Microphone ({device})");
    let (mut writer, reader) = pcm_ring(48_000, 960, 4800);
    writer.push(&[0.0; 960]);
    let microphone =
        VirtualMicrophone::spawn(&device, reader, VoiceChannels::Mono).expect("node is created");
    let appeared = wait_for(|| nodes_listing().is_some_and(|nodes| nodes.contains(&description)));
    eprintln!(
        "pw-cli ls Node shows {:?}: {appeared}",
        microphone.node_name()
    );
    assert!(appeared, "virtual microphone node is listed");
    drop(microphone);
    let gone = wait_for(|| nodes_listing().is_some_and(|nodes| !nodes.contains(&description)));
    assert!(gone, "virtual microphone node is removed on drop");
}
