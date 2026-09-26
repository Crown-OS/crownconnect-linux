use llts_node::MediaRequest;
use llts_signaling::device::DeviceId;
use llts_signaling::message::StreamRef;

use crate::peer::media_coordinator::MediaCoordinator;

/// The streams whose pipelines run, and the coordinator that runs them.
#[derive(Debug)]
pub(super) struct ActiveStreams {
    coordinator: Box<dyn MediaCoordinator>,
    running: Vec<(DeviceId, StreamRef)>,
}

impl ActiveStreams {
    pub(super) fn new(coordinator: Box<dyn MediaCoordinator>) -> Self {
        Self {
            coordinator,
            running: Vec::new(),
        }
    }

    pub(super) fn handle(&mut self, request: &MediaRequest, peer_name: &str) {
        match *request {
            MediaRequest::Start { peer, stream, .. } => self.running.push((peer, stream)),
            MediaRequest::Stop { peer, stream, .. } => {
                if !self.is_running(&peer, stream) {
                    tracing::debug!(?stream, "stopping a stream that never started");
                }
                self.running.retain(|running| *running != (peer, stream));
            }
        }
        self.coordinator.handle(request, peer_name);
        tracing::debug!(streams = self.running.len(), "media streams");
    }

    pub(super) fn coordinator(&mut self) -> &mut dyn MediaCoordinator {
        &mut *self.coordinator
    }

    pub(super) fn is_running(&self, peer: &DeviceId, stream: StreamRef) -> bool {
        self.running.contains(&(*peer, stream))
    }
}

#[cfg(test)]
mod tests {
    use llts_node::{FeatureRequest, MediaRole};
    use llts_signaling::device::Feature;
    use llts_signaling::message::{AudioCodec, AudioParams, MuxClass, StreamParams};

    use super::*;
    use crate::peer::media_coordinator::LoggingMediaCoordinator;

    #[test]
    fn streams_run_between_start_and_stop() {
        let mut streams = ActiveStreams::new(Box::new(LoggingMediaCoordinator));
        let peer = DeviceId([2; 32]);
        let stream = StreamRef {
            class: MuxClass::Audio,
            index: 0,
        };
        streams.handle(
            &MediaRequest::Start {
                peer,
                request: FeatureRequest::Mic,
                role: MediaRole::Decoder,
                stream,
                params: StreamParams::Audio(AudioParams {
                    codec: AudioCodec::Opus,
                    sample_rate: 48_000,
                    channels: 1,
                    frame_ms: 10,
                }),
            },
            "peer",
        );
        assert!(streams.is_running(&peer, stream));
        streams.handle(
            &MediaRequest::Stop {
                peer,
                feature: Feature::Mic,
                stream,
            },
            "peer",
        );
        assert!(!streams.is_running(&peer, stream));
    }
}
