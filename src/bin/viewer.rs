//! Shows a mirrored phone screen in a window.
//!
//! The daemon pipes framed access units (see `media::present::access_unit`) into stdin; frames
//! are decoded on the GPU and their dmabufs attached to the window without copies. Pointer,
//! keyboard and touch input goes back on stdout as framed `InputMessage`s.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use crownconnect_linux::media::present::access_unit::{Fill, UnitReader};
use crownconnect_linux::media::present::input::write_message;
use crownconnect_linux::media::present::{PresentationStats, Presenter};
use crownconnect_linux::media::video::{VaapiDevice, VideoCodec, VideoDecoder};
use crownconnect_linux::util::latency::{LatencySummary, LatencyWindow};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const STATS_INTERVAL: Duration = Duration::from_secs(2);
const DRAIN_AFTER_EOF: Duration = Duration::from_millis(500);
const LATENCY_SAMPLES: usize = 600;

struct Options {
    codec: VideoCodec,
    title: String,
}

fn parse_options() -> Result<Options> {
    let mut options = Options {
        codec: VideoCodec::Hevc,
        title: "Phone screen".to_owned(),
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .with_context(|| format!("{argument} needs a value"))
        };
        match argument.as_str() {
            "--codec" => {
                options.codec = match value()?.as_str() {
                    "hevc" | "h265" => VideoCodec::Hevc,
                    "h264" | "avc" => VideoCodec::H264,
                    "av1" => VideoCodec::Av1,
                    other => bail!("unknown codec {other}"),
                };
            }
            "--title" => options.title = value()?,
            other => bail!("unknown argument {other}; usage: crownconnect-viewer [--codec hevc|h264|av1] [--title T]"),
        }
    }
    Ok(options)
}

fn nonblocking_stdin() -> Result<File> {
    let stdin = std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .context("duplicating stdin")?;
    let flags = fcntl_getfl(&stdin).context("reading stdin flags")?;
    fcntl_setfl(&stdin, flags | OFlags::NONBLOCK).context("making stdin non-blocking")?;
    Ok(File::from(stdin))
}

fn describe(summary: Option<LatencySummary>) -> String {
    summary.map_or_else(
        || "n/a".to_owned(),
        |summary| format!("median {:?} p99 {:?}", summary.median, summary.p99),
    )
}

fn report(decode: &LatencyWindow, stats: &PresentationStats) {
    eprintln!(
        "viewer: {} presented, {} discarded, {} dmabuf imports, {} held; decode+submit {}; \
         submit->present {}",
        stats.presented,
        stats.discarded,
        stats.imports,
        stats.held,
        describe(decode.summary()),
        describe(stats.submit_to_present),
    );
}

fn main() -> Result<()> {
    let options = parse_options()?;
    let device = VaapiDevice::open_default().context("opening the VA-API render node")?;
    let mut decoder = VideoDecoder::new(&device, options.codec).context("opening the decoder")?;
    let mut presenter = Presenter::connect(&options.title).context("opening the window")?;
    let mut input = nonblocking_stdin()?;
    let stdout = std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .context("duplicating stdout")?;
    let mut output = BufWriter::new(File::from(stdout));
    let mut reader = UnitReader::default();
    let mut decode_latency = LatencyWindow::with_capacity(LATENCY_SAMPLES);
    let mut stream_ended: Option<Instant> = None;
    let mut last_report = Instant::now();

    while !presenter.is_closed()
        && stream_ended.is_none_or(|ended| ended.elapsed() < DRAIN_AFTER_EOF)
    {
        let watch_stdin = stream_ended.is_none().then(|| input.as_fd());
        if presenter.wait(watch_stdin, Some(POLL_INTERVAL))? {
            if reader.fill_from(&mut input)? == Fill::Closed {
                stream_ended = Some(Instant::now());
            }
            while let Some((header, unit)) = reader.next_unit()? {
                let received = Instant::now();
                decoder.decode(unit, header.pts)?;
                while let Some(frame) = decoder.next_frame()? {
                    presenter.present(frame)?;
                    decode_latency.record(received.elapsed());
                }
            }
        }
        for message in presenter.drain_input() {
            write_message(&mut output, &message)?;
        }
        output.flush()?;
        if last_report.elapsed() >= STATS_INTERVAL {
            report(&decode_latency, &presenter.stats());
            last_report = Instant::now();
        }
    }
    report(&decode_latency, &presenter.stats());
    Ok(())
}
