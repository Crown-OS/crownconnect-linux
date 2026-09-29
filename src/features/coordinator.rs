//! The media coordinator the peer runtime drives: one pipeline thread per negotiated stream
//! (and per shared cursor), frames and rate changes routed to them, their output queued for the
//! runtime to put on the wire.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

use llts_core::session::Action;
use llts_core::stream::StreamKey;
use llts_node::{FeatureRequest, MediaRequest, MediaRole, RemoteCommand};
use llts_signaling::device::{DeviceId, Feature};
use llts_signaling::message::{MuxClass, StreamParams, StreamRef};
use llts_wire::StreamClass;

use super::input_route::{connect_injector, InputOwner, InputRoutes, SharedInjector};
use super::pipeline::{Control, Outlet, PipelineContext, PipelineHandle, PipelineSpec};
use super::platform::{InputForwarder, MediaPlatform, MirrorWindow};
use super::{camera, mic, mirror, monitor, unicursor, FeatureError};
use crate::ipc::proto::Edge;
use crate::peer::{MediaCoordinator, MediaOutput, RuntimeWaker, UnicursorSignal};

/// Room for a few frames of every stream while the runtime thread catches up.
const OUTPUT_QUEUE_DEPTH: usize = 256;

type PipelineBody = Box<dyn FnOnce(&PipelineContext) -> Result<(), FeatureError> + Send>;

/// Runs the media features of every paired device on this computer's hardware.
#[derive(Debug)]
pub struct FeatureCoordinator {
    platform: Arc<dyn MediaPlatform>,
    waker: Option<RuntimeWaker>,
    outputs: Receiver<MediaOutput>,
    output_sender: SyncSender<MediaOutput>,
    pipelines: Vec<PipelineHandle>,
    /// Stopped pipelines whose threads are still winding down.
    retired: Vec<JoinHandle<()>>,
    input: InputRoutes,
}

fn stream_ref(key: StreamKey) -> Option<StreamRef> {
    MuxClass::from_byte(key.class.as_byte()).map(|class| StreamRef {
        class,
        index: key.index,
    })
}

const fn input_owner(feature: Feature) -> Option<InputOwner> {
    match feature {
        Feature::Mirror => Some(InputOwner::Mirror),
        Feature::Monitor => Some(InputOwner::Monitor),
        _ => None,
    }
}

impl FeatureCoordinator {
    pub fn new(platform: Arc<dyn MediaPlatform>) -> Self {
        let (output_sender, outputs) = sync_channel(OUTPUT_QUEUE_DEPTH);
        Self {
            platform,
            waker: None,
            outputs,
            output_sender,
            pipelines: Vec::new(),
            retired: Vec::new(),
            input: InputRoutes::default(),
        }
    }

    fn outlet(&self) -> Outlet {
        Outlet::new(self.output_sender.clone(), self.waker.clone())
    }

    fn fail(&self, peer: DeviceId, feature: Feature, error: &FeatureError) {
        self.outlet().send(MediaOutput::Failed {
            peer,
            feature,
            reason: error.to_string(),
        });
    }

    fn spawn(&mut self, spec: PipelineSpec, body: PipelineBody) {
        match PipelineHandle::spawn(spec, self.outlet(), body) {
            Ok(pipeline) => self.pipelines.push(pipeline),
            Err(error) => self.fail(spec.peer, spec.feature, &FeatureError::Io(error)),
        }
    }

    fn start(
        &mut self,
        peer: DeviceId,
        request: FeatureRequest,
        role: MediaRole,
        stream: StreamRef,
        params: StreamParams,
        peer_name: &str,
    ) {
        let feature = request.feature();
        let platform = Arc::clone(&self.platform);
        let spec = |name| PipelineSpec {
            name,
            peer,
            feature,
            stream: Some(stream),
        };
        let (spec, body): (PipelineSpec, PipelineBody) = match (request, role, params) {
            (FeatureRequest::Mirror(mirror), MediaRole::Encoder, StreamParams::Video(video)) => {
                if mirror.remote_input {
                    let injector = connect_injector(Arc::clone(&platform), None);
                    self.input.add(peer, InputOwner::Mirror, injector);
                }
                (
                    spec("crownconnect-mirror-out"),
                    Box::new(move |context| mirror::share_screen(context, &*platform, &video)),
                )
            }
            (
                FeatureRequest::Mirror(_) | FeatureRequest::Monitor(_),
                MediaRole::Decoder,
                StreamParams::Video(video),
            ) => {
                let takes_input = match request {
                    FeatureRequest::Mirror(mirror) => mirror.remote_input,
                    _ => true,
                };
                let input = takes_input.then(|| InputForwarder::new(self.outlet(), peer));
                let peer_name = peer_name.to_owned();
                (
                    spec("crownconnect-mirror-in"),
                    Box::new(move |context| {
                        let window = MirrorWindow {
                            params: &video,
                            peer_name: &peer_name,
                        };
                        mirror::show_peer_screen(context, &*platform, window, input)
                    }),
                )
            }
            (FeatureRequest::Monitor(mode), MediaRole::Encoder, StreamParams::Video(video)) => {
                let injector = SharedInjector::default();
                self.input
                    .add(peer, InputOwner::Monitor, Arc::clone(&injector));
                (
                    spec("crownconnect-monitor"),
                    Box::new(move |context| {
                        monitor::extend_desktop(context, &*platform, &video, &mode, &injector)
                    }),
                )
            }
            (FeatureRequest::Camera(_), MediaRole::Decoder, StreamParams::Video(video)) => (
                spec("crownconnect-camera"),
                Box::new(move |context| camera::show_as_camera(context, &*platform, &video)),
            ),
            (FeatureRequest::Mic, MediaRole::Decoder, StreamParams::Audio(_)) => {
                let peer_name = peer_name.to_owned();
                (
                    spec("crownconnect-mic-in"),
                    Box::new(move |context| {
                        let mut sink = platform.voice_sink(&peer_name)?;
                        mic::run_decoder(context, &mut *sink)
                    }),
                )
            }
            (FeatureRequest::Mic, MediaRole::Encoder, StreamParams::Audio(_)) => (
                spec("crownconnect-mic-out"),
                Box::new(move |context| {
                    let mut source = platform.voice_source()?;
                    mic::run_encoder(context, &mut *source)
                }),
            ),
            _ => {
                return self.fail(
                    peer,
                    feature,
                    &FeatureError::Unsupported("this half of the feature"),
                );
            }
        };
        self.spawn(spec, body);
    }

    fn stop(&mut self, peer: DeviceId, feature: Feature, stream: Option<StreamRef>) {
        let (stopped, running) =
            std::mem::take(&mut self.pipelines)
                .into_iter()
                .partition(|pipeline| {
                    pipeline.peer == peer
                        && pipeline.feature == feature
                        && pipeline.stream == stream
                });
        self.pipelines = running;
        self.retired
            .extend(stopped.into_iter().filter_map(PipelineHandle::retire));
        if let Some(owner) = input_owner(feature) {
            self.input.remove(peer, owner);
        }
    }

    fn pipeline(
        &self,
        peer: DeviceId,
        feature: Option<Feature>,
        stream: Option<StreamRef>,
    ) -> Option<&PipelineHandle> {
        self.pipelines.iter().find(|pipeline| {
            pipeline.peer == peer
                && feature.is_none_or(|feature| pipeline.feature == feature)
                && (stream.is_none() || pipeline.stream == stream)
        })
    }

    fn control(&self, peer: DeviceId, stream: StreamKey, control: Control) {
        let Some(pipeline) = self.pipeline(peer, None, stream_ref(stream)) else {
            return;
        };
        let carried_frame = matches!(control, Control::Frame(_));
        if pipeline.send(control) {
            return;
        }
        tracing::debug!(?stream, "the pipeline is behind; dropping");
        if carried_frame && let Some(stream) = stream_ref(stream) {
            self.outlet()
                .send(MediaOutput::KeyframeNeeded { peer, stream });
        }
    }

    fn reap(&mut self) {
        let (finished, running): (Vec<_>, Vec<_>) = std::mem::take(&mut self.retired)
            .into_iter()
            .partition(JoinHandle::is_finished);
        self.retired = running;
        for thread in finished {
            let _ = thread.join();
        }
    }
}

impl MediaCoordinator for FeatureCoordinator {
    fn attach(&mut self, waker: RuntimeWaker) {
        self.waker = Some(waker);
    }

    fn handle(&mut self, request: &MediaRequest, peer_name: &str) {
        match *request {
            MediaRequest::Start {
                peer,
                request,
                role,
                stream,
                params,
            } => {
                tracing::info!(%peer, feature = ?request.feature(), ?role, ?stream, "starting a pipeline");
                self.stop(peer, request.feature(), Some(stream));
                self.start(peer, request, role, stream, params, peer_name);
            }
            MediaRequest::Stop {
                peer,
                feature,
                stream,
            } => {
                tracing::info!(%peer, ?feature, ?stream, "stopping a pipeline");
                self.stop(peer, feature, Some(stream));
            }
        }
    }

    fn on_session_action(&mut self, peer: DeviceId, action: Action) {
        match action {
            Action::PresentFrame { stream, frame } => {
                self.control(peer, stream, Control::Frame(frame));
            }
            Action::Deliver { stream, message } if stream.class == StreamClass::Input => {
                if let Some(leave) = self.input.deliver(peer, message.filled()) {
                    self.outlet().send(MediaOutput::Unicursor {
                        peer,
                        signal: UnicursorSignal::Leave(leave),
                    });
                }
            }
            Action::InputKeyState { state, .. } => self.input.key_state(peer, state.filled()),
            Action::SetTargetBitrate { stream, bitrate } => {
                self.control(
                    peer,
                    stream,
                    Control::TargetBitrate(bitrate.bits_per_second()),
                );
            }
            Action::SetFrameByteBudget { stream, bytes } => {
                self.control(peer, stream, Control::FrameByteBudget(bytes));
            }
            Action::RequestKeyframe { stream } => self.control(peer, stream, Control::Keyframe),
            _ => {}
        }
    }

    fn on_connection(&mut self, peer: DeviceId, connected: bool) {
        if !connected {
            self.stop(peer, Feature::Unicursor, None);
            self.input.remove_peer(peer);
        }
    }

    fn on_remote_command(&mut self, peer: DeviceId, command: &RemoteCommand) {
        match *command {
            RemoteCommand::UnicursorEnter(enter) => {
                if !self.input.contains(peer, InputOwner::RemoteCursor) {
                    let injector = connect_injector(Arc::clone(&self.platform), None);
                    self.input.add(peer, InputOwner::RemoteCursor, injector);
                }
                self.input.cursor_entered(peer, enter);
            }
            RemoteCommand::UnicursorLeave(leave) => {
                if let Some(pipeline) = self.pipeline(peer, Some(Feature::Unicursor), None) {
                    pipeline.send(Control::UnicursorLeave(leave));
                }
            }
            _ => {}
        }
    }

    fn set_unicursor_edge(&mut self, peer: DeviceId, edge: Edge) -> Result<(), String> {
        self.stop(peer, Feature::Unicursor, None);
        let platform = Arc::clone(&self.platform);
        self.spawn(
            PipelineSpec {
                name: "crownconnect-unicursor",
                peer,
                feature: Feature::Unicursor,
                stream: None,
            },
            Box::new(move |context| unicursor::run_capture(context, &*platform, edge)),
        );
        Ok(())
    }

    fn poll_output(&mut self) -> Option<MediaOutput> {
        self.reap();
        self.outputs.try_recv().ok()
    }
}
