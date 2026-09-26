//! Everything a pipeline opens on this computer — screens, windows, cameras, microphones and the
//! seat — behind one trait, so tests can run the same pipelines against synthetic devices.

use std::path::{Path, PathBuf};

use llts_signaling::device::DeviceId;
use llts_signaling::message::VideoParams;

use super::camera::CameraSink;
use super::mic::{CapturedVoice, VirtualVoice};
use super::pipeline::Outlet;
use super::screen::{CaptureTarget, ScreenSource};
use super::sink::{VideoSink, VoiceSink, VoiceSource};
use super::source::FrameSource;
use super::viewer::ViewerSink;
use super::FeatureError;
use crate::config::DaemonConfig;
use crate::peer::MediaOutput;
use crate::wayland::injector::Injector;
use crate::wayland::input_capture::{CaptureEvent, InputCapture};
use crate::wayland::EventOutlet;

/// Sends a viewer's input to the peer whose screen it shows.
#[derive(Debug, Clone)]
pub struct InputForwarder {
    outlet: Outlet,
    peer: DeviceId,
}

impl InputForwarder {
    pub(crate) const fn new(outlet: Outlet, peer: DeviceId) -> Self {
        Self { outlet, peer }
    }

    pub fn send_batch(&self, events: Vec<u8>) {
        self.outlet.send(MediaOutput::Input {
            peer: self.peer,
            events,
        });
    }

    pub fn send_key_state(&self, keys: Vec<u8>) {
        self.outlet.send(MediaOutput::KeyState {
            peer: self.peer,
            keys,
        });
    }
}

/// What a mirrored screen is shown as.
#[derive(Debug, Clone, Copy)]
pub struct MirrorWindow<'a> {
    pub params: &'a VideoParams,
    pub peer_name: &'a str,
}

/// Opens the devices pipelines run on. Every method defaults to this computer's real devices;
/// called on pipeline threads, so each may block for a compositor or PipeWire roundtrip.
pub trait MediaPlatform: Send + Sync + std::fmt::Debug {
    /// # Errors
    ///
    /// Fails without screencast support or a hardware encoder for the stream's codec.
    fn screen_source(
        &self,
        target: CaptureTarget,
        params: &VideoParams,
    ) -> Result<Box<dyn FrameSource>, FeatureError> {
        Ok(Box::new(ScreenSource::open(target, params)?))
    }

    /// A window showing a peer's screen; its input goes to `input` when the peer takes it.
    ///
    /// # Errors
    ///
    /// Fails when the viewer cannot be started.
    fn mirror_sink(
        &self,
        window: MirrorWindow<'_>,
        input: Option<InputForwarder>,
    ) -> Result<Box<dyn VideoSink>, FeatureError> {
        let title = format!("{} screen", window.peer_name);
        Ok(Box::new(ViewerSink::spawn(
            window.params.codec,
            &title,
            input,
        )?))
    }

    /// The v4l2loopback device the user chose; `None` takes the first one there is.
    fn camera_device(&self) -> Option<&Path> {
        None
    }

    /// # Errors
    ///
    /// Fails without a v4l2loopback device or a hardware decoder.
    fn camera_sink(&self, params: &VideoParams) -> Result<Box<dyn VideoSink>, FeatureError> {
        Ok(Box::new(CameraSink::open(params, self.camera_device())?))
    }

    /// # Errors
    ///
    /// Fails without PipeWire.
    fn voice_source(&self) -> Result<Box<dyn VoiceSource>, FeatureError> {
        Ok(Box::new(CapturedVoice::open()?))
    }

    /// # Errors
    ///
    /// Fails without PipeWire.
    fn voice_sink(&self, peer_name: &str) -> Result<Box<dyn VoiceSink>, FeatureError> {
        Ok(Box::new(VirtualVoice::open(peer_name)?))
    }

    /// Injects into this computer's seat, absolute positions landing on `output`.
    ///
    /// # Errors
    ///
    /// Fails without crownos-input-v1.
    fn injector(&self, output: Option<String>) -> Result<Injector, FeatureError> {
        Ok(Injector::connect(output)?)
    }

    /// # Errors
    ///
    /// Fails without crownos-input-v1.
    fn input_capture(
        &self,
        events: EventOutlet<CaptureEvent>,
    ) -> Result<InputCapture, FeatureError> {
        Ok(InputCapture::connect(events)?)
    }
}

/// This computer's real devices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinuxPlatform {
    camera_device: Option<PathBuf>,
}

impl LinuxPlatform {
    pub fn new(config: &DaemonConfig) -> Self {
        Self {
            camera_device: config.camera_device.clone(),
        }
    }
}

impl MediaPlatform for LinuxPlatform {
    fn camera_device(&self) -> Option<&Path> {
        self.camera_device.as_deref()
    }
}
