//! Carrying out what the node asks for.

use llts_node::bridge::execute;
use llts_node::{
    AppEvent, DriverCommand, FeatureFailure, FfspLinkExport, LinkEvent, MediaRequest, NodeInput,
    NodeOutput, PersistRequest, Presence,
};
use llts_signaling::discovery::Beacon;
use llts_signaling::state::Topic;

use super::event_loop::PeerLoop;
use crate::ipc::proto::PairingOffer;
use crate::ipc::server::{DaemonEvent, OwnedUpdate};
use crate::pairing::ffsp_bridge::{derive_ffsp_link_secret, FfspLink};
use crate::peer::convert::{ipc_feature, ipc_feature_state, ipc_id};
use crate::util::error_chain::error_chain;

impl PeerLoop {
    pub(super) fn on_output(&mut self, output: NodeOutput) {
        match output {
            NodeOutput::Driver(command) => self.execute(command),
            NodeOutput::App(event) => self.on_app_event(event),
            NodeOutput::Media(request) => self.start_or_stop_media(&request),
            NodeOutput::Persist(request) => self.persist(request),
            NodeOutput::ExportFfspLink(export) => self.export_ffsp_link(&export),
            NodeOutput::Advertise(beacons) => self.advertise(beacons),
        }
    }

    fn execute(&mut self, command: DriverCommand) {
        match execute(&mut self.driver, command) {
            Ok(Some(offer)) => {
                let now = self.clock.now();
                self.node.handle(
                    now,
                    NodeInput::Link(LinkEvent::QrOfferReady { offer: &offer }),
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(error = %error_chain(&error), "llts driver refused a command");
                self.failures.push(error);
            }
        }
    }

    fn on_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::DevicesChanged => self.sync_devices(),
            AppEvent::DeviceConnected { peer, connected } => {
                self.streams.coordinator().on_connection(peer, connected);
                self.sync_devices();
            }
            AppEvent::PairingOffer {
                uri,
                expires_unix_ms,
            } => match self.pending.pairing_offer.take() {
                Some(reply) => reply.reply(PairingOffer {
                    qr: uri,
                    expires_unix_ms,
                }),
                None => tracing::debug!("a pairing offer nobody asked for"),
            },
            AppEvent::PairingRequest { peer, name, code } => {
                self.announce(DaemonEvent::PairingRequest {
                    id: ipc_id(peer),
                    name,
                    code: format!("{code:06}"),
                });
            }
            AppEvent::PairingResult { peer, accepted } => {
                if let Some(reply) = self.pending.qr_pairing.take() {
                    if accepted {
                        reply.reply(());
                    } else {
                        reply.fail("the pairing failed or was refused");
                    }
                }
                if let Some(peer) = peer {
                    self.announce(DaemonEvent::PairingResult {
                        id: ipc_id(peer),
                        accepted,
                    });
                }
            }
            AppEvent::PairingWindowClosed => tracing::debug!("pairing window closed"),
            AppEvent::PairableDeviceSeen { class, address, .. } => {
                tracing::debug!(?class, ?address, "a device nearby is pairing");
            }
            AppEvent::RemoteStateChanged { peer, topic } => self.on_remote_state(peer, topic),
            AppEvent::FeatureStateChanged {
                peer,
                feature,
                state,
            } => self.announce(DaemonEvent::FeatureState {
                id: ipc_id(peer),
                feature: ipc_feature(feature),
                state: ipc_feature_state(state),
            }),
            AppEvent::FeatureFailed {
                peer,
                feature,
                reason,
            } => {
                tracing::warn!(%peer, ?feature, ?reason, "feature did not start");
                self.announce(DaemonEvent::FeatureFailed {
                    id: ipc_id(peer),
                    feature: ipc_feature(feature),
                    reason: failure_reason(reason).to_owned(),
                });
                if let Some(state) = self.node.feature_state(&peer, feature) {
                    self.announce(DaemonEvent::FeatureState {
                        id: ipc_id(peer),
                        feature: ipc_feature(feature),
                        state: ipc_feature_state(state),
                    });
                }
            }
            AppEvent::RemoteCommand { peer, command } => self.on_remote_command(peer, command),
            AppEvent::Error(error) => {
                tracing::warn!(error = %error_chain(&error), "llts node");
                self.failures.push(error);
            }
        }
    }

    fn start_or_stop_media(&mut self, request: &MediaRequest) {
        let peer = match request {
            MediaRequest::Start { peer, .. } | MediaRequest::Stop { peer, .. } => peer,
        };
        let name = self
            .node
            .device(peer)
            .map(|device| device.name)
            .unwrap_or_default();
        self.streams.handle(request, name);
    }

    fn on_remote_state(&mut self, peer: llts_signaling::device::DeviceId, topic: Topic) {
        if let Some(update) = self.node.remote_update(&peer, topic) {
            let update = OwnedUpdate {
                topic: update.topic,
                version: update.version,
                payload: update.payload.to_vec(),
            };
            self.announce(DaemonEvent::StateUpdate {
                owner: ipc_id(peer),
                update,
            });
        }
        match topic {
            Topic::Clipboard => self.apply_newest_clipboard(),
            Topic::DeviceInfo | Topic::Battery => self.sync_devices(),
            _ => {}
        }
    }

    fn persist(&mut self, request: PersistRequest) {
        let outcome = match request {
            PersistRequest::PeerFeatures { peer, enabled } => self.features.set(peer, enabled),
            PersistRequest::ForgetPeer { peer } => {
                if let Err(error) = self.ffsp.forget(&peer.0) {
                    tracing::warn!(%error, "ffsp still trusts a forgotten device");
                }
                self.features.remove(&peer)
            }
        };
        if let Err(error) = outcome {
            tracing::warn!(error = %error_chain(&error), "cannot save feature choices");
        }
    }

    fn export_ffsp_link(&self, export: &FfspLinkExport) {
        let link = FfspLink {
            public_key: export.peer.0,
            name: &export.name,
            class: export.class,
            secret: derive_ffsp_link_secret(export.link_secret.as_bytes()),
        };
        match self.ffsp.record(&link) {
            Ok(()) => tracing::info!(peer = %export.peer, "ffsp now trusts the device too"),
            Err(error) => tracing::warn!(%error, "cannot hand the device to ffsp"),
        }
    }

    /// Static peers see nothing this computer advertises, but a linked beacon names the pair,
    /// not the sender, so this computer's own beacons are what those peers would advertise.
    fn advertise(&mut self, beacons: Vec<Beacon>) {
        if !self.static_peers.is_empty() {
            for beacon in beacons
                .iter()
                .filter(|beacon| matches!(beacon, Beacon::Linked(_)))
            {
                let now = self.clock.now();
                self.node.handle(
                    now,
                    NodeInput::Presence(Presence::Seen {
                        beacon: *beacon,
                        addresses: &self.static_peers,
                    }),
                );
            }
        }
        self.services.advertiser.advertise(beacons);
    }
}

const fn failure_reason(failure: FeatureFailure) -> &'static str {
    match failure {
        FeatureFailure::Unavailable => "the feature is turned off or unsupported on one end",
        FeatureFailure::NoCommonCodec => "no codec both devices support",
        FeatureFailure::Refused => "the device refused or stopped it",
        FeatureFailure::AlreadyActive => "it is already running",
        FeatureFailure::NotConnected => "the device is not connected",
    }
}
