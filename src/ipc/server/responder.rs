use std::marker::PhantomData;

use crownos_ipc::{PeerId, RemoteError};
use serde::Serialize;
use tokio::sync::mpsc;

/// A reply produced after the request that asked for it was dispatched.
#[derive(Debug)]
pub(crate) struct DeferredReply {
    pub(crate) peer: PeerId,
    pub(crate) req_id: u32,
    pub(crate) outcome: Result<Vec<u8>, RemoteError>,
}

#[derive(Debug)]
struct ReplyTarget {
    peer: PeerId,
    req_id: u32,
    replies: mpsc::UnboundedSender<DeferredReply>,
}

impl ReplyTarget {
    fn send(self, outcome: Result<Vec<u8>, RemoteError>) {
        let _ = self.replies.send(DeferredReply {
            peer: self.peer,
            req_id: self.req_id,
            outcome,
        });
    }
}

/// Answers one IPC request later, from wherever the work happens. A responder dropped without
/// answering replies with an error, so no client is left waiting.
pub struct Responder<R> {
    target: Option<ReplyTarget>,
    _reply: PhantomData<fn(R)>,
}

impl<R> std::fmt::Debug for Responder<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Responder")
            .field("target", &self.target)
            .finish()
    }
}

impl<R: Serialize> Responder<R> {
    pub(crate) const fn new(
        peer: PeerId,
        req_id: u32,
        replies: mpsc::UnboundedSender<DeferredReply>,
    ) -> Self {
        Self {
            target: Some(ReplyTarget {
                peer,
                req_id,
                replies,
            }),
            _reply: PhantomData,
        }
    }

    pub fn respond(mut self, result: Result<R, RemoteError>) {
        if let Some(target) = self.target.take() {
            target.send(result.and_then(|reply| {
                postcard::to_stdvec(&reply)
                    .map_err(|_| RemoteError::handler("reply serialization failed"))
            }));
        }
    }

    pub fn reply(self, reply: R) {
        self.respond(Ok(reply));
    }

    pub fn fail(self, message: impl Into<String>) {
        self.respond(Err(RemoteError::handler(message)));
    }
}

impl<R> Drop for Responder<R> {
    fn drop(&mut self) {
        if let Some(target) = self.target.take() {
            target.send(Err(RemoteError::handler(
                "the peer runtime dropped the request",
            )));
        }
    }
}
