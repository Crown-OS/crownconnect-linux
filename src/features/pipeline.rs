//! One media pipeline on its own thread: the controls the runtime sends it, the outlet it
//! reports through, and the handle that stops it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use llts_core::receive::AssembledFrame;
use llts_signaling::device::{DeviceId, Feature};
use llts_signaling::message::{StreamRef, UnicursorLeave};

use super::FeatureError;
use crate::peer::{EncodedFrame, MediaOutput, RuntimeWaker, UnicursorSignal};
use crate::util::error_chain::error_chain;

/// Frames waiting for a decoder beyond this are dropped rather than queued, so latency stays
/// bounded when a decoder falls behind.
const CONTROL_QUEUE_DEPTH: usize = 16;

/// What the runtime tells a running pipeline.
#[derive(Debug)]
pub(crate) enum Control {
    Frame(AssembledFrame),
    TargetBitrate(u32),
    FrameByteBudget(u32),
    Keyframe,
    UnicursorLeave(UnicursorLeave),
}

/// Where pipelines hand their output to the runtime thread, waking it.
#[derive(Debug, Clone)]
pub(crate) struct Outlet {
    outputs: SyncSender<MediaOutput>,
    waker: Option<RuntimeWaker>,
}

impl Outlet {
    pub(crate) const fn new(outputs: SyncSender<MediaOutput>, waker: Option<RuntimeWaker>) -> Self {
        Self { outputs, waker }
    }

    /// `false` when the runtime is too far behind to take it, or gone.
    pub(crate) fn send(&self, output: MediaOutput) -> bool {
        let sent = self.outputs.try_send(output).is_ok();
        if let Some(waker) = &self.waker {
            waker.wake();
        }
        sent
    }
}

/// What a pipeline thread knows about itself.
#[derive(Debug)]
pub(crate) struct PipelineContext {
    pub(crate) peer: DeviceId,
    pub(crate) feature: Feature,
    pub(crate) stream: Option<StreamRef>,
    pub(crate) outlet: Outlet,
    controls: Receiver<Control>,
    stop: Arc<AtomicBool>,
}

impl PipelineContext {
    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// The next control, waiting at most `timeout`; `Err` once the pipeline should end.
    pub(crate) fn next_control(&self, timeout: Duration) -> Result<Option<Control>, Stopped> {
        if self.stopped() {
            return Err(Stopped);
        }
        match self.controls.recv_timeout(timeout) {
            Ok(control) => Ok(Some(control)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(Stopped),
        }
    }

    /// A control that is already waiting; `Err` once the pipeline should end.
    pub(crate) fn pending_control(&self) -> Result<Option<Control>, Stopped> {
        if self.stopped() {
            return Err(Stopped);
        }
        match self.controls.try_recv() {
            Ok(control) => Ok(Some(control)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(Stopped),
        }
    }

    /// `false` when the frame was dropped, after which the next one should be a keyframe.
    pub(crate) fn send_frame(&self, frame: EncodedFrame) -> bool {
        let Some(stream) = self.stream else {
            return false;
        };
        self.outlet.send(MediaOutput::Frame {
            peer: self.peer,
            stream,
            frame,
        })
    }

    pub(crate) fn send_input(&self, events: Vec<u8>) {
        self.outlet.send(MediaOutput::Input {
            peer: self.peer,
            events,
        });
    }

    pub(crate) fn send_key_state(&self, keys: Vec<u8>) {
        self.outlet.send(MediaOutput::KeyState {
            peer: self.peer,
            keys,
        });
    }

    pub(crate) fn send_unicursor(&self, signal: UnicursorSignal) {
        self.outlet.send(MediaOutput::Unicursor {
            peer: self.peer,
            signal,
        });
    }

    fn fail(&self, error: &FeatureError) {
        self.outlet.send(MediaOutput::Failed {
            peer: self.peer,
            feature: self.feature,
            reason: error_chain(error),
        });
    }
}

/// The pipeline was told to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stopped;

/// A running pipeline. Dropping it asks the thread to stop without waiting for it.
#[derive(Debug)]
pub(crate) struct PipelineHandle {
    pub(crate) peer: DeviceId,
    pub(crate) feature: Feature,
    pub(crate) stream: Option<StreamRef>,
    controls: SyncSender<Control>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// What identifies a pipeline to start.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PipelineSpec {
    pub(crate) name: &'static str,
    pub(crate) peer: DeviceId,
    pub(crate) feature: Feature,
    pub(crate) stream: Option<StreamRef>,
}

impl PipelineHandle {
    /// Runs `body` on a new thread; an error it returns before being stopped is reported as the
    /// feature failing.
    pub(crate) fn spawn(
        spec: PipelineSpec,
        outlet: Outlet,
        body: impl FnOnce(&PipelineContext) -> Result<(), FeatureError> + Send + 'static,
    ) -> std::io::Result<Self> {
        let (controls, received) = sync_channel(CONTROL_QUEUE_DEPTH);
        let stop = Arc::new(AtomicBool::new(false));
        let context = PipelineContext {
            peer: spec.peer,
            feature: spec.feature,
            stream: spec.stream,
            outlet,
            controls: received,
            stop: Arc::clone(&stop),
        };
        let thread = std::thread::Builder::new()
            .name(spec.name.to_owned())
            .spawn(move || {
                if let Err(error) = body(&context)
                    && !context.stopped()
                {
                    context.fail(&error);
                }
            })?;
        Ok(Self {
            peer: spec.peer,
            feature: spec.feature,
            stream: spec.stream,
            controls,
            stop,
            thread: Some(thread),
        })
    }

    /// `false` when the pipeline is too far behind to take it.
    pub(crate) fn send(&self, control: Control) -> bool {
        self.controls.try_send(control).is_ok()
    }

    /// Asks the thread to stop and hands it over, to be joined once it finished.
    pub(crate) fn retire(mut self) -> Option<JoinHandle<()>> {
        self.stop.store(true, Ordering::Release);
        self.thread.take()
    }
}

impl Drop for PipelineHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
