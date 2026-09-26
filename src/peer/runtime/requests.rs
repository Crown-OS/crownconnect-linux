//! Turning IPC requests and notices into node commands, and answering them.

use std::path::Path;

use llts_node::{AppCommand, FeatureRequest, NodeInput, OutgoingCommand, Presence};
use llts_signaling::device::DeviceId;
use llts_signaling::message::{FfspTransferId, FileOffer, Media, Reply, SetHotspot};
use llts_signaling::state::{MediaSession, Notifications};

use super::calls::CallControl;
use super::event_loop::PeerLoop;
use crate::discovery::{AddressHint, PresenceEvent};
use crate::features::files::{start_ffsp_send, FfspSend};
use crate::ipc::proto::FeatureSet;
use crate::ipc::server::Responder;
use crate::peer::convert::{
    signaling_feature, signaling_id, start_camera, start_mirror, start_monitor,
};
use crate::peer::link_monitor::dialable;
use crate::peer::{PeerCommand, PeerNotice, PeerRequest};
use crate::util::error_chain::error_chain;
use crate::util::random::random_u32;

impl PeerLoop {
    pub(super) fn on_command(&mut self, command: PeerCommand) {
        match command {
            PeerCommand::Request(request) => self.on_request(request),
            PeerCommand::Notice(notice) => self.on_notice(notice),
            PeerCommand::LocalState(state) => self.publish(state),
            PeerCommand::Presence(presence) => self.on_presence(&presence),
            PeerCommand::Calls(calls) => self.on_phone_calls(calls),
        }
    }

    /// Hands `command` to the node and reports the first error it caused.
    pub(super) fn run_app(&mut self, command: AppCommand<'_>) -> Result<(), String> {
        self.failures.clear();
        let now = self.clock.now();
        self.node.handle(now, NodeInput::App(command));
        self.settle();
        self.failures
            .drain(..)
            .next()
            .map_or(Ok(()), |error| Err(error_chain(&error)))
    }

    pub(super) fn send_command(
        &mut self,
        peer: DeviceId,
        command: OutgoingCommand<'_>,
    ) -> Result<(), String> {
        self.run_app(AppCommand::Send { peer, command })
    }

    pub(super) fn respond(reply: Responder<()>, outcome: Result<(), String>) {
        match outcome {
            Ok(()) => reply.reply(()),
            Err(reason) => reply.fail(reason),
        }
    }

    fn start(&mut self, peer: DeviceId, request: FeatureRequest, reply: Responder<()>) {
        let outcome = self.run_app(AppCommand::StartFeature { peer, request });
        Self::respond(reply, outcome);
    }

    fn on_request(&mut self, request: PeerRequest) {
        match request {
            PeerRequest::PairingBegin(reply) => {
                self.pending.pairing_offer = Some(reply);
                let outcome = self.run_app(AppCommand::BeginPairing { duration: None });
                if let Some(reply) = self.pending.pairing_offer.take() {
                    reply.fail(
                        outcome
                            .err()
                            .unwrap_or_else(|| "no pairing offer".to_owned()),
                    );
                }
            }
            PeerRequest::PairWithQr { qr, reply } => {
                self.pending.qr_pairing = Some(reply);
                if let Err(reason) = self.run_app(AppCommand::PairWithQr { uri: &qr })
                    && let Some(reply) = self.pending.qr_pairing.take()
                {
                    reply.fail(reason);
                }
            }
            PeerRequest::PairingConfirm { id, accept, reply } => {
                let outcome = self.run_app(AppCommand::ConfirmPairing {
                    peer: signaling_id(id),
                    accept,
                });
                Self::respond(reply, outcome);
            }
            PeerRequest::Forget { id, reply } => {
                let outcome = self.run_app(AppCommand::Forget {
                    peer: signaling_id(id),
                });
                Self::respond(reply, outcome);
            }
            PeerRequest::SendFile { id, path, reply } => {
                self.send_file(signaling_id(id), &path, reply)
            }
            PeerRequest::StartMirror { id, options, reply } => self.start(
                signaling_id(id),
                FeatureRequest::Mirror(start_mirror(options)),
                reply,
            ),
            PeerRequest::StartCamera { id, options, reply } => self.start(
                signaling_id(id),
                FeatureRequest::Camera(start_camera(options)),
                reply,
            ),
            PeerRequest::StartMic { id, reply } => {
                self.start(signaling_id(id), FeatureRequest::Mic, reply);
            }
            PeerRequest::StartMonitor { id, options, reply } => self.start(
                signaling_id(id),
                FeatureRequest::Monitor(start_monitor(options)),
                reply,
            ),
            PeerRequest::Stop { id, feature, reply } => {
                let outcome = self.run_app(AppCommand::StopFeature {
                    peer: signaling_id(id),
                    feature: signaling_feature(feature),
                });
                Self::respond(reply, outcome);
            }
            PeerRequest::SetUnicursorEdge { id, edge, reply } => {
                let peer = signaling_id(id);
                let outcome = if self
                    .node
                    .device(&peer)
                    .is_some_and(|device| device.connected)
                {
                    self.streams.coordinator().set_unicursor_edge(peer, edge)
                } else {
                    Err(format!("{peer} is not connected"))
                };
                Self::respond(reply, outcome);
            }
            PeerRequest::PickupCall { call_id, reply } => {
                self.control_call(CallControl::Pickup, &call_id, reply);
            }
            PeerRequest::DeclineCall { call_id, reply } => {
                self.control_call(CallControl::Decline, &call_id, reply);
            }
            PeerRequest::HangupCall { call_id, reply } => {
                self.control_call(CallControl::Hangup, &call_id, reply);
            }
            PeerRequest::Dial { id, number, reply } => self.dial(signaling_id(id), &number, reply),
            PeerRequest::SetHotspot { id, enabled, reply } => {
                let outcome = self.send_command(
                    signaling_id(id),
                    OutgoingCommand::SetHotspot(SetHotspot { enabled }),
                );
                Self::respond(reply, outcome);
            }
        }
    }

    /// Starts the ffsp sender when one is installed and offers the file over llts.
    fn send_file(&mut self, peer: DeviceId, path: &str, reply: Responder<()>) {
        if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
            return reply.fail(format!("{path} is not a readable file"));
        }
        match start_ffsp_send(Path::new(path)) {
            Ok(FfspSend::Started) => tracing::info!(path, "ffsp is sending the file"),
            Ok(FfspSend::Unavailable) => {
                tracing::info!("no fileshare-linux installed; the device only sees the offer");
            }
            Err(error) => return reply.fail(format!("cannot start the ffsp sender: {error}")),
        }
        let transfer = match random_u32() {
            Ok(id) => FfspTransferId(id),
            Err(error) => return reply.fail(error.to_string()),
        };
        tracing::info!(%peer, path, transfer = transfer.0, "offering a file");
        let outcome = self.send_command(
            peer,
            OutgoingCommand::FileOffer(FileOffer {
                ffsp_transfer_id: transfer,
            }),
        );
        Self::respond(reply, outcome);
    }

    fn on_notice(&mut self, notice: PeerNotice) {
        let outcome = match notice {
            PeerNotice::PairingCancel => self.run_app(AppCommand::CancelPairing),
            PeerNotice::Media(media) => self.control_media(media),
            PeerNotice::SendReply {
                conversation_id,
                text,
            } => self.send_reply(&conversation_id, &text),
            PeerNotice::FeaturesChanged { id, features } => {
                self.set_features(signaling_id(id), features)
            }
        };
        if let Err(reason) = outcome {
            tracing::warn!(reason, "cannot act on a notification");
        }
    }

    /// Media keys go to the device that is playing, or failing that one with a player open.
    fn control_media(&mut self, media: Media) -> Result<(), String> {
        let target = self
            .node
            .devices()
            .filter(|device| device.connected)
            .filter_map(|device| {
                let session = self
                    .node
                    .remote_state::<Option<MediaSession<'_>>>(&device.id)
                    .ok()
                    .flatten()
                    .flatten()?;
                Some((session.playing, device.id))
            })
            .max_by_key(|(playing, _)| *playing)
            .map(|(_, id)| id)
            .ok_or("no connected device has a player open")?;
        self.send_command(target, OutgoingCommand::Media(media))
    }

    /// A reply goes to the device showing the notification it answers.
    pub(super) fn send_reply(&mut self, conversation_id: &str, text: &str) -> Result<(), String> {
        let target = self
            .node
            .devices()
            .find(|device| {
                self.node
                    .remote_state::<Notifications<'_>>(&device.id)
                    .ok()
                    .flatten()
                    .is_some_and(|notifications| {
                        notifications
                            .active
                            .iter()
                            .any(|notification| notification.id == conversation_id)
                    })
            })
            .map(|device| device.id)
            .ok_or_else(|| format!("no device shows conversation {conversation_id}"))?;
        self.send_command(
            target,
            OutgoingCommand::Reply(Reply {
                conversation_id,
                text,
            }),
        )
    }

    fn set_features(&mut self, peer: DeviceId, features: FeatureSet) -> Result<(), String> {
        let enabled = self
            .node
            .device(&peer)
            .map(|device| device.enabled)
            .ok_or_else(|| format!("{peer} is not paired"))?;
        crate::ipc::proto::Feature::ALL
            .into_iter()
            .filter(|feature| {
                features.contains(*feature) != enabled.contains(signaling_feature(*feature))
            })
            .try_for_each(|feature| {
                self.run_app(AppCommand::SetFeature {
                    peer,
                    feature: signaling_feature(feature),
                    enabled: features.contains(feature),
                })
            })
    }

    fn on_presence(&mut self, presence: &PresenceEvent) {
        let now = self.clock.now();
        match presence {
            PresenceEvent::Seen { beacon, hint, .. } => {
                let addresses: Vec<_> = match hint {
                    AddressHint::Lan { addresses, port } => addresses
                        .iter()
                        .filter(|ip| dialable(**ip))
                        .map(|ip| std::net::SocketAddr::new(*ip, *port))
                        .collect(),
                    AddressHint::Ble(_) => Vec::new(),
                };
                self.node.handle(
                    now,
                    NodeInput::Presence(Presence::Seen {
                        beacon: *beacon,
                        addresses: &addresses,
                    }),
                );
            }
            PresenceEvent::Lost { beacon, .. } => {
                self.node
                    .handle(now, NodeInput::Presence(Presence::Lost { beacon: *beacon }));
            }
        }
    }
}
