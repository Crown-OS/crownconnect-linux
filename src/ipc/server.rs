//! The `crownconnect` IPC service: answers what the daemon knows itself, forwards what needs a
//! paired device to the peer runtime, and broadcasts what the runtime reports.

mod convert;
mod daemon_state;
mod driver;
mod outgoing;
mod responder;

use std::path::Path;

use crownos_ipc::{Kind, Message, MethodMsg, PeerId, RemoteError, Server, ServiceBuilder};
use llts_signaling::message::Media;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::mpsc;

pub use daemon_state::{DaemonEvent, OwnedUpdate};
pub use responder::Responder;

use crate::ipc::proto;
use crate::peer::{PeerCommand, PeerNotice, PeerRequest};
use daemon_state::DaemonState;
use driver::SocketDriver;
use responder::DeferredReply;

const PEER_BUSY: &str = "peer runtime unavailable: its queue is full or it has stopped";

#[derive(Debug, thiserror::Error)]
pub enum IpcServerError {
    #[error("crownconnect IPC: {0}")]
    Ipc(#[from] crownos_ipc::Error),
    #[error("crownconnect IPC: {0}")]
    Io(#[from] std::io::Error),
}

/// The bound service, ready to [`run`](Self::run).
pub struct IpcServer {
    server: Server,
    router: Router,
    replies: mpsc::UnboundedReceiver<DeferredReply>,
    daemon_events: mpsc::Receiver<DaemonEvent>,
}

impl std::fmt::Debug for IpcServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IpcServer")
            .field("service", &self.server.service())
            .field("router", &self.router)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Router {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Router")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl IpcServer {
    /// Binds the socket, in `directory` or crownos-ipc's default.
    ///
    /// # Errors
    ///
    /// Fails when the socket cannot be bound, as when another daemon owns it.
    pub fn bind(
        directory: Option<&Path>,
        peer: mpsc::Sender<PeerCommand>,
        daemon_events: mpsc::Receiver<DaemonEvent>,
    ) -> Result<Self, IpcServerError> {
        let builder = ServiceBuilder::new(proto::SERVICE);
        let server = match directory {
            Some(directory) => builder.directory(directory).build()?,
            None => builder.build()?,
        };
        let (reply_sender, replies) = mpsc::unbounded_channel();
        Ok(Self {
            server,
            router: Router {
                state: DaemonState::default(),
                peer,
                replies: reply_sender,
            },
            replies,
            daemon_events,
        })
    }

    /// Serves until the peer runtime closes its event channel.
    ///
    /// # Errors
    ///
    /// Fails when the listening socket itself breaks.
    pub async fn run(mut self) -> Result<(), IpcServerError> {
        let mut driver = SocketDriver::new(&self.server)?;
        loop {
            tokio::select! {
                readiness = driver.ready(&self.server) => {
                    let router = &mut self.router;
                    driver.process(&mut self.server, readiness, |server, peer, message| {
                        router.dispatch(server, peer, &message);
                    })?;
                }
                Some(reply) = self.replies.recv() => send_deferred(&mut self.server, reply),
                event = self.daemon_events.recv() => match event {
                    Some(event) => {
                        for outgoing in self.router.state.apply(event) {
                            outgoing.emit(&mut self.server)?;
                        }
                    }
                    None => return Ok(()),
                },
            }
        }
    }
}

fn send_deferred(server: &mut Server, reply: DeferredReply) {
    let _ = match reply.outcome {
        Ok(payload) => server.reply(reply.peer, reply.req_id, &payload, Vec::new()),
        Err(error) => server.reply_err(reply.peer, reply.req_id, &error),
    };
}

struct Router {
    state: DaemonState,
    peer: mpsc::Sender<PeerCommand>,
    replies: mpsc::UnboundedSender<DeferredReply>,
}

/// A request that could not be decoded; the sender is dropped, as the generated dispatch does.
struct Malformed;

impl From<postcard::Error> for Malformed {
    fn from(_: postcard::Error) -> Self {
        Self
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, RemoteError> {
    postcard::to_stdvec(value).map_err(|_| RemoteError::handler("reply serialization failed"))
}

fn args<M: MethodMsg<Args = M> + DeserializeOwned>(payload: &[u8]) -> Result<M, Malformed> {
    Ok(postcard::from_bytes(payload)?)
}

impl Router {
    fn dispatch(&mut self, server: &mut Server, peer: PeerId, message: &Message) {
        let routed = match message.kind {
            Kind::Request => self.request(server, peer, message),
            Kind::Notify => self.notice(message.selector, &message.payload),
            _ => Err(Malformed),
        };
        if routed.is_err() {
            server.drop_peer(peer);
        }
    }

    fn responder<R: Serialize>(&self, peer: PeerId, req_id: u32) -> Responder<R> {
        Responder::new(peer, req_id, self.replies.clone())
    }

    fn forward(&self, request: PeerRequest) {
        if let Err(error) = self.peer.try_send(PeerCommand::Request(request))
            && let PeerCommand::Request(request) = error.into_inner()
        {
            request.reject(PEER_BUSY);
        }
    }

    fn notify(&self, notice: PeerNotice) {
        if self.peer.try_send(PeerCommand::Notice(notice)).is_err() {
            tracing::warn!("peer runtime busy; dropping a notification");
        }
    }

    fn request(
        &mut self,
        server: &mut Server,
        peer: PeerId,
        message: &Message,
    ) -> Result<(), Malformed> {
        use proto::*;
        let payload = message.payload.as_slice();
        let req_id = message.req_id;
        let reply = |server: &mut Server, result: Result<Vec<u8>, RemoteError>| {
            send_deferred(
                server,
                DeferredReply {
                    peer,
                    req_id,
                    outcome: result,
                },
            );
        };
        match message.selector {
            <devices as MethodMsg>::SELECTOR => {
                args::<devices>(payload)?;
                reply(server, encode(&self.state.devices()));
            }
            <features as MethodMsg>::SELECTOR => {
                let call = args::<features>(payload)?;
                reply(
                    server,
                    self.state.features(call.id).and_then(|set| encode(&set)),
                );
            }
            <state as MethodMsg>::SELECTOR => {
                let call = args::<state>(payload)?;
                reply(
                    server,
                    self.state
                        .state(call.id, call.topic)
                        .and_then(|doc| encode(&doc)),
                );
            }
            <set_feature as MethodMsg>::SELECTOR => {
                let call = args::<set_feature>(payload)?;
                match self.state.set_feature(call.id, call.feature, call.enabled) {
                    Ok((features, event)) => {
                        reply(server, encode(&()));
                        let _ = event.emit(server);
                        self.notify(PeerNotice::FeaturesChanged {
                            id: call.id,
                            features,
                        });
                    }
                    Err(error) => reply(server, Err(error)),
                }
            }
            <pairing_begin as MethodMsg>::SELECTOR => {
                args::<pairing_begin>(payload)?;
                self.forward(PeerRequest::PairingBegin(self.responder(peer, req_id)));
            }
            <pairing_confirm as MethodMsg>::SELECTOR => {
                let call = args::<pairing_confirm>(payload)?;
                self.forward(PeerRequest::PairingConfirm {
                    id: call.id,
                    accept: call.accept,
                    reply: self.responder(peer, req_id),
                });
            }
            <forget as MethodMsg>::SELECTOR => {
                let call = args::<forget>(payload)?;
                self.forward(PeerRequest::Forget {
                    id: call.id,
                    reply: self.responder(peer, req_id),
                });
            }
            <send_file as MethodMsg>::SELECTOR => {
                let call = args::<send_file>(payload)?;
                self.forward(PeerRequest::SendFile {
                    id: call.id,
                    path: call.path,
                    reply: self.responder(peer, req_id),
                });
            }
            <start_mirror as MethodMsg>::SELECTOR => {
                let call = args::<start_mirror>(payload)?;
                self.forward(PeerRequest::StartMirror {
                    id: call.id,
                    options: call.options,
                    reply: self.responder(peer, req_id),
                });
            }
            <start_camera as MethodMsg>::SELECTOR => {
                let call = args::<start_camera>(payload)?;
                self.forward(PeerRequest::StartCamera {
                    id: call.id,
                    options: call.options,
                    reply: self.responder(peer, req_id),
                });
            }
            <start_mic as MethodMsg>::SELECTOR => {
                let call = args::<start_mic>(payload)?;
                self.forward(PeerRequest::StartMic {
                    id: call.id,
                    reply: self.responder(peer, req_id),
                });
            }
            <start_monitor as MethodMsg>::SELECTOR => {
                let call = args::<start_monitor>(payload)?;
                self.forward(PeerRequest::StartMonitor {
                    id: call.id,
                    options: call.options,
                    reply: self.responder(peer, req_id),
                });
            }
            <stop as MethodMsg>::SELECTOR => {
                let call = args::<stop>(payload)?;
                self.forward(PeerRequest::Stop {
                    id: call.id,
                    feature: call.feature,
                    reply: self.responder(peer, req_id),
                });
            }
            <set_unicursor_edge as MethodMsg>::SELECTOR => {
                let call = args::<set_unicursor_edge>(payload)?;
                self.forward(PeerRequest::SetUnicursorEdge {
                    id: call.id,
                    edge: call.edge,
                    reply: self.responder(peer, req_id),
                });
            }
            <pickup_call as MethodMsg>::SELECTOR => {
                let call = args::<pickup_call>(payload)?;
                self.forward(PeerRequest::PickupCall {
                    call_id: call.call_id,
                    reply: self.responder(peer, req_id),
                });
            }
            <decline_call as MethodMsg>::SELECTOR => {
                let call = args::<decline_call>(payload)?;
                self.forward(PeerRequest::DeclineCall {
                    call_id: call.call_id,
                    reply: self.responder(peer, req_id),
                });
            }
            <hangup_call as MethodMsg>::SELECTOR => {
                let call = args::<hangup_call>(payload)?;
                self.forward(PeerRequest::HangupCall {
                    call_id: call.call_id,
                    reply: self.responder(peer, req_id),
                });
            }
            <dial as MethodMsg>::SELECTOR => {
                let call = args::<dial>(payload)?;
                self.forward(PeerRequest::Dial {
                    id: call.id,
                    number: call.number,
                    reply: self.responder(peer, req_id),
                });
            }
            <set_hotspot as MethodMsg>::SELECTOR => {
                let call = args::<set_hotspot>(payload)?;
                self.forward(PeerRequest::SetHotspot {
                    id: call.id,
                    enabled: call.enabled,
                    reply: self.responder(peer, req_id),
                });
            }
            <pair_with_qr as MethodMsg>::SELECTOR => {
                let call = args::<pair_with_qr>(payload)?;
                self.forward(PeerRequest::PairWithQr {
                    qr: call.qr,
                    reply: self.responder(peer, req_id),
                });
            }
            selector => reply(server, Err(RemoteError::unknown_method(selector))),
        }
        Ok(())
    }

    fn notice(&self, selector: u64, payload: &[u8]) -> Result<(), Malformed> {
        use proto::*;
        let notice = match selector {
            <pairing_cancel as MethodMsg>::SELECTOR => {
                args::<pairing_cancel>(payload)?;
                PeerNotice::PairingCancel
            }
            <media_play as MethodMsg>::SELECTOR => {
                args::<media_play>(payload)?;
                PeerNotice::Media(Media::Play)
            }
            <media_pause as MethodMsg>::SELECTOR => {
                args::<media_pause>(payload)?;
                PeerNotice::Media(Media::Pause)
            }
            <media_next as MethodMsg>::SELECTOR => {
                args::<media_next>(payload)?;
                PeerNotice::Media(Media::Next)
            }
            <media_previous as MethodMsg>::SELECTOR => {
                args::<media_previous>(payload)?;
                PeerNotice::Media(Media::Previous)
            }
            <media_seek as MethodMsg>::SELECTOR => PeerNotice::Media(Media::Seek {
                percent: args::<media_seek>(payload)?.percent,
            }),
            <send_reply as MethodMsg>::SELECTOR => {
                let call = args::<send_reply>(payload)?;
                PeerNotice::SendReply {
                    conversation_id: call.conversation_id,
                    text: call.text,
                }
            }
            _ => return Ok(()),
        };
        self.notify(notice);
        Ok(())
    }
}
