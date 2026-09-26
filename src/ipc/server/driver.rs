use std::collections::HashMap;
use std::future::poll_fn;
use std::os::fd::{AsFd, OwnedFd};
use std::task::Poll;

use crownos_ipc::{Message, PeerId, Server};
use tokio::io::unix::AsyncFd;
use tokio::io::{Interest, Ready};

/// Which sockets can make progress.
#[derive(Debug, Default)]
pub(super) struct Readiness {
    accept: bool,
    peers: Vec<(PeerId, bool, bool)>,
}

/// Pumps a crownos-ipc [`Server`] from tokio readiness, like the crate's own tokio adapter,
/// but as a future that can be raced against the daemon's other inputs.
pub(super) struct SocketDriver {
    listener: AsyncFd<OwnedFd>,
    peers: HashMap<PeerId, AsyncFd<OwnedFd>>,
}

fn watch(fd: impl AsFd) -> std::io::Result<AsyncFd<OwnedFd>> {
    AsyncFd::with_interest(
        fd.as_fd().try_clone_to_owned()?,
        Interest::READABLE | Interest::WRITABLE,
    )
}

impl SocketDriver {
    pub(super) fn new(server: &Server) -> std::io::Result<Self> {
        Ok(Self {
            listener: watch(server)?,
            peers: HashMap::new(),
        })
    }

    pub(super) async fn ready(&self, server: &Server) -> Readiness {
        poll_fn(|context| {
            let mut readiness = Readiness::default();
            if let Poll::Ready(Ok(mut guard)) = self.listener.poll_read_ready(context) {
                guard.clear_ready_matching(Ready::READABLE);
                readiness.accept = true;
            }
            for (&id, watcher) in &self.peers {
                let Some(interest) = server.peer_interest(id) else {
                    readiness.peers.push((id, false, false));
                    continue;
                };
                let mut readable = false;
                let mut writable = false;
                if interest.read
                    && let Poll::Ready(Ok(mut guard)) = watcher.poll_read_ready(context)
                {
                    guard.clear_ready_matching(Ready::READABLE);
                    readable = true;
                }
                if interest.write
                    && let Poll::Ready(Ok(mut guard)) = watcher.poll_write_ready(context)
                {
                    guard.clear_ready_matching(Ready::WRITABLE);
                    writable = true;
                }
                if readable || writable {
                    readiness.peers.push((id, readable, writable));
                }
            }
            if readiness.accept || !readiness.peers.is_empty() {
                Poll::Ready(readiness)
            } else {
                Poll::Pending
            }
        })
        .await
    }

    /// Accepts, flushes and reads whatever `readiness` covers, handing each request to
    /// `dispatch`.
    pub(super) fn process(
        &mut self,
        server: &mut Server,
        readiness: Readiness,
        mut dispatch: impl FnMut(&mut Server, PeerId, Message),
    ) -> Result<(), crownos_ipc::Error> {
        if readiness.accept {
            while let Some(id) = server.accept()? {
                if let Some(fd) = server.peer_fd(id) {
                    self.peers.insert(id, watch(fd)?);
                }
            }
        }
        for (id, readable, writable) in readiness.peers {
            if server.peer_interest(id).is_none()
                || (writable && server.handle_writable(id).is_err())
                || (readable && server.handle_readable(id).is_err())
            {
                self.peers.remove(&id);
                continue;
            }
            while let Some(message) = server.next_request(id) {
                dispatch(server, id, message);
            }
            if server.reap(id) || server.peer_interest(id).is_none() {
                self.peers.remove(&id);
            }
        }
        Ok(())
    }
}
