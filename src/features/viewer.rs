//! A peer's screen in a `crownconnect-viewer` window: access units go down its stdin, and the
//! pointer, keyboard and touch input it sees comes back on its stdout for the peer.

use std::io::{BufReader, ErrorKind, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;

use llts_signaling::message::VideoCodec;

use super::codec::viewer_codec;
use super::input_wire::{BatchWriter, PressedKeys};
use super::platform::InputForwarder;
use super::sink::{ReceivedFrame, VideoSink};
use super::FeatureError;
use crate::media::present::access_unit::UnitHeader;
use crate::media::present::input::{read_message, InputEvent};

const VIEWER_BINARY: &str = "crownconnect-viewer";
const VIEWER_OVERRIDE: &str = "CROWNCONNECT_VIEWER";

/// `CROWNCONNECT_VIEWER`, else the viewer installed next to the daemon, else the one on `PATH`.
fn viewer_binary() -> PathBuf {
    if let Some(path) = std::env::var_os(VIEWER_OVERRIDE) {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .map(|daemon| daemon.with_file_name(VIEWER_BINARY))
        .filter(|sibling| sibling.is_file())
        .unwrap_or_else(|| PathBuf::from(VIEWER_BINARY))
}

#[derive(Debug)]
pub(crate) struct ViewerSink {
    child: Child,
    stdin: ChildStdin,
    input: Option<JoinHandle<()>>,
}

impl ViewerSink {
    pub(crate) fn spawn(
        codec: VideoCodec,
        title: &str,
        input: Option<InputForwarder>,
    ) -> Result<Self, FeatureError> {
        let mut child = Command::new(viewer_binary())
            .args(["--codec", viewer_codec(codec), "--title", title])
            .stdin(Stdio::piped())
            .stdout(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take().ok_or(FeatureError::ViewerClosed)?;
        let input = match (input, child.stdout.take()) {
            (Some(forwarder), Some(stdout)) => Some(
                std::thread::Builder::new()
                    .name("crownconnect-viewer-input".to_owned())
                    .spawn(move || forward_input(stdout, &forwarder))?,
            ),
            _ => None,
        };
        Ok(Self {
            child,
            stdin,
            input,
        })
    }
}

impl VideoSink for ViewerSink {
    fn present(&mut self, frame: &ReceivedFrame<'_>) -> Result<(), FeatureError> {
        let header = UnitHeader {
            len: u32::try_from(frame.bytes.len())
                .map_err(|_| FeatureError::Unsupported("a frame over 4 GiB"))?,
            pts: i64::from(frame.timestamp),
            keyframe: frame.keyframe,
        };
        self.stdin
            .write_all(&header.encode())
            .and_then(|()| self.stdin.write_all(frame.bytes))
            .map_err(|error| match error.kind() {
                ErrorKind::BrokenPipe => FeatureError::ViewerClosed,
                _ => FeatureError::Io(error),
            })
    }

    fn check(&mut self) -> Result<(), FeatureError> {
        match self.child.try_wait()? {
            Some(_) => Err(FeatureError::ViewerClosed),
            None => Ok(()),
        }
    }
}

impl Drop for ViewerSink {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(input) = self.input.take() {
            let _ = input.join();
        }
    }
}

/// Batches the viewer's input per moment and forwards it, with a key-state snapshot whenever
/// the held keys change, until the viewer exits.
fn forward_input(stdout: ChildStdout, forwarder: &InputForwarder) {
    let mut reader = BufReader::new(stdout);
    let mut batch = BatchWriter::default();
    let mut keys = PressedKeys::default();
    while let Ok(Some(message)) = read_message(&mut reader) {
        let (flush, keys_changed) = match message.event {
            InputEvent::Key { code, pressed } => (true, keys.note(code, pressed)),
            InputEvent::Frame => (true, false),
            _ => (false, false),
        };
        batch.push(message.time_ms, message.event);
        if flush && let Some(events) = batch.take() {
            forwarder.send_batch(events);
        }
        if keys_changed {
            forwarder.send_key_state(keys.snapshot());
        }
    }
}
