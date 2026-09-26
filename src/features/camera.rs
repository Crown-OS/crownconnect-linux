//! A peer's camera as a camera on this computer: decoded on the GPU, read back once as NV12 and
//! written to a v4l2loopback device that any camera app can open.

use std::path::{Path, PathBuf};

use llts_signaling::message::VideoParams;

use super::codec::media_codec;
use super::pipeline::PipelineContext;
use super::platform::MediaPlatform;
use super::sink::{run_decoder, ReceivedFrame, VideoSink};
use super::FeatureError;
use crate::media::v4l2_sink::{FrameSink, SinkPixelFormat, V4l2LoopbackSink};
use crate::media::video::{DownloadBuffer, VaapiDevice, VideoDecoder};

const VIDEO_DEVICES: &str = "/dev";
const VIDEO_DEVICE_PREFIX: &str = "video";
const LOOPBACK_DRIVER: &str = "v4l2 loopback";

/// Whether the device at `path` is a v4l2loopback node.
fn is_loopback(path: &Path) -> bool {
    v4l::Device::with_path(path)
        .and_then(|device| device.query_caps())
        .is_ok_and(|caps| caps.driver == LOOPBACK_DRIVER)
}

/// The configured device if it is a loopback one, else the first loopback device there is.
pub(crate) fn loopback_device(configured: Option<&Path>) -> Result<PathBuf, FeatureError> {
    if let Some(path) = configured {
        return if is_loopback(path) {
            Ok(path.to_path_buf())
        } else {
            Err(FeatureError::NoLoopbackCamera)
        };
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(VIDEO_DEVICES)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(VIDEO_DEVICE_PREFIX))
        })
        .map(|entry| entry.path())
        .collect();
    candidates.sort_unstable();
    candidates
        .into_iter()
        .find(|path| is_loopback(path))
        .ok_or(FeatureError::NoLoopbackCamera)
}

/// Shows the peer's camera on the loopback device until stopped.
pub(crate) fn show_as_camera(
    context: &PipelineContext,
    platform: &dyn MediaPlatform,
    params: &VideoParams,
) -> Result<(), FeatureError> {
    let mut sink = platform.camera_sink(params)?;
    run_decoder(context, &mut *sink)
}

#[derive(Debug)]
pub(crate) struct CameraSink {
    path: PathBuf,
    decoder: VideoDecoder,
    download: DownloadBuffer,
    output: Option<V4l2LoopbackSink>,
    _device: VaapiDevice,
}

impl CameraSink {
    /// # Errors
    ///
    /// Fails without a v4l2loopback device or a hardware decoder for the stream's codec.
    pub(crate) fn open(
        params: &VideoParams,
        configured_device: Option<&Path>,
    ) -> Result<Self, FeatureError> {
        let path = loopback_device(configured_device)?;
        let device = VaapiDevice::open_default()?;
        let decoder = VideoDecoder::new(&device, media_codec(params.codec))?;
        Ok(Self {
            path,
            decoder,
            download: DownloadBuffer::new()?,
            output: None,
            _device: device,
        })
    }
}

impl VideoSink for CameraSink {
    fn present(&mut self, frame: &ReceivedFrame<'_>) -> Result<(), FeatureError> {
        self.decoder
            .decode(frame.bytes, i64::from(frame.timestamp))?;
        while let Some(decoded) = self.decoder.next_frame()? {
            let image = decoded.download(&mut self.download)?;
            let output = match self.output.take() {
                Some(output) => output,
                None => V4l2LoopbackSink::open(
                    &self.path,
                    image.width,
                    image.height,
                    SinkPixelFormat::Yuyv,
                )?,
            };
            self.output.insert(output).write_frame(&image)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_that_is_not_a_loopback_camera_is_refused() {
        assert!(matches!(
            loopback_device(Some(Path::new("/dev/null"))),
            Err(FeatureError::NoLoopbackCamera)
        ));
    }
}
