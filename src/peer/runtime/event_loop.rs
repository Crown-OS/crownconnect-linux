//! The runtime thread's loop: sleep until the socket, a command or a timer needs attention,
//! then let the driver and the node catch up with each other.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;

use llts_node::bridge::link_event;
use llts_node::{Node, NodeError, NodeInput};
use llts_signaling::state::HybridTimestamp;
use llts_transport::pairing::FileTrustStore;
use llts_transport::session::{Driver, DriverEvent};
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::io::Errno;
use tokio::sync::mpsc;

use super::active_streams::ActiveStreams;
use super::calls::HandsFreeCalls;
use super::clock::RuntimeClock;
use super::PeerRuntimeError;
use crate::bluetooth::telephony::TelephonyJob;
use crate::discovery::Advertiser;
use crate::ipc::proto::{DeviceId, DeviceInfo, PairingOffer};
use crate::ipc::server::{DaemonEvent, Responder};
use crate::pairing::ffsp_bridge::FfspLinkStore;
use crate::peer::feature_store::FeatureStore;
use crate::peer::link_monitor::LinkMonitor;
use crate::peer::wakeup::Wakeup;
use crate::peer::PeerCommand;
use crate::state::LocalControls;
use crate::util::error_chain::error_chain;

/// The node always has a timer within a discovery epoch; this only bounds a clock jump.
const LONGEST_SLEEP: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(super) struct Services {
    pub(super) daemon_events: mpsc::Sender<DaemonEvent>,
    pub(super) controls: LocalControls,
    pub(super) advertiser: Advertiser,
    pub(super) telephony: mpsc::Sender<TelephonyJob>,
}

/// IPC requests answered by a later node event rather than by the command itself.
#[derive(Debug, Default)]
pub(super) struct PendingReplies {
    pub(super) pairing_offer: Option<Responder<PairingOffer>>,
    pub(super) qr_pairing: Option<Responder<()>>,
}

pub(super) struct PeerLoop {
    pub(super) driver: Driver<FileTrustStore>,
    pub(super) node: Node,
    pub(super) clock: RuntimeClock,
    pub(super) services: Services,
    pub(super) streams: ActiveStreams,
    pub(super) features: FeatureStore,
    pub(super) ffsp: FfspLinkStore,
    pub(super) static_peers: Vec<SocketAddr>,
    pub(super) links: LinkMonitor,
    pub(super) hands_free: HandsFreeCalls,
    /// The device list as last announced to the IPC server.
    pub(super) listed: BTreeMap<DeviceId, DeviceInfo>,
    pub(super) pending: PendingReplies,
    /// Errors the node or driver reported since the current command started.
    pub(super) failures: Vec<NodeError>,
    pub(super) applied_clipboard: Option<HybridTimestamp>,
    pub(super) commands: std_mpsc::Receiver<PeerCommand>,
    pub(super) wakeup: Arc<Wakeup>,
    pub(super) stopping: bool,
}

/// Which of the loop's descriptors are readable.
#[derive(Debug, Clone, Copy, Default)]
struct Ready {
    commands: bool,
    addresses: bool,
}

impl PeerLoop {
    pub(super) fn run(mut self) -> Result<(), PeerRuntimeError> {
        self.announce_network();
        self.sync_devices();
        while !self.stopping {
            let ready = self.wait()?;
            if ready.commands {
                self.wakeup.clear();
            }
            if let Err(error) = self.driver.poll() {
                tracing::debug!(error = %error_chain(&error), "llts socket");
            }
            if ready.addresses && self.links.refresh() {
                self.announce_network();
            }
            self.receive_commands();
            self.drain_media();
            self.run_due_timers();
            self.settle();
        }
        Ok(())
    }

    pub(super) fn announce(&mut self, event: DaemonEvent) {
        if self.services.daemon_events.blocking_send(event).is_err() {
            self.stopping = true;
        }
    }

    /// Feeds driver events to the node and carries out the node's outputs until neither has
    /// anything left, since each can produce more of the other.
    pub(super) fn settle(&mut self) {
        loop {
            while let Some(output) = self.node.poll_output() {
                self.on_output(output);
            }
            let Some(event) = self.driver.poll_event() else {
                return;
            };
            self.on_driver_event(event);
        }
    }

    fn on_driver_event(&mut self, event: DriverEvent) {
        if let DriverEvent::Connected { peer, .. } = &event {
            self.open_input(peer);
        }
        match link_event(&event) {
            Some(link) => {
                let now = self.clock.now();
                self.node.handle(now, NodeInput::Link(link));
            }
            None => self.route_to_media(event),
        }
    }

    fn announce_network(&mut self) {
        let now = self.clock.now();
        tracing::debug!(addresses = ?self.links.addresses(), "reachable at");
        self.node.handle(
            now,
            NodeInput::NetworkChanged {
                addresses: self.links.addresses(),
                rebind: None,
            },
        );
        self.settle();
    }

    fn receive_commands(&mut self) {
        loop {
            match self.commands.try_recv() {
                Ok(command) => {
                    self.on_command(command);
                    self.settle();
                }
                Err(std_mpsc::TryRecvError::Empty) => return,
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    self.stopping = true;
                    return;
                }
            }
        }
    }

    fn run_due_timers(&mut self) {
        let now = self.clock.now();
        if self
            .node
            .next_timeout()
            .is_some_and(|deadline| !deadline.is_after(now.instant))
        {
            self.node.handle(now, NodeInput::TimerExpired);
        }
    }

    fn sleep_limit(&self) -> Duration {
        let node = self
            .node
            .next_timeout()
            .map(|deadline| self.clock.until(deadline));
        [self.driver.next_timeout(), node]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(LONGEST_SLEEP)
            .min(LONGEST_SLEEP)
    }

    fn wait(&self) -> Result<Ready, PeerRuntimeError> {
        let timeout = Timespec::try_from(self.sleep_limit()).ok();
        // SAFETY: the driver owns this socket and outlives the borrow, which ends with `poll`.
        let socket = unsafe { BorrowedFd::borrow_raw(self.driver.as_raw_fd()) };
        let mut descriptors = [
            PollFd::from_borrowed_fd(socket, PollFlags::IN),
            PollFd::new(&*self.wakeup, PollFlags::IN),
            PollFd::new(&self.links, PollFlags::IN),
        ];
        match poll(&mut descriptors, timeout.as_ref()) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        let readable = |index: usize| {
            descriptors
                .get(index)
                .is_some_and(|descriptor| !descriptor.revents().is_empty())
        };
        Ok(Ready {
            commands: readable(1),
            addresses: readable(2),
        })
    }
}
