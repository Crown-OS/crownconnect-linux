//! The peer runtime: one OS thread owning the llts socket and the node that drives it.
//!
//! The thread sleeps in `poll(2)` on the socket, an eventfd that tokio tasks ring when they
//! queue a [`PeerCommand`], and a netlink socket for address changes, with the earliest driver
//! or node timer as the timeout. Everything it learns goes back as [`DaemonEvent`]s; platform
//! work goes to [`LocalControls`], the [`Advertiser`] and the telephony task, none of which
//! block it.

mod active_streams;
mod calls;
mod clock;
mod devices;
mod event_loop;
mod local;
mod media;
mod outputs;
mod requests;
mod setup;

use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use tokio::sync::{mpsc, oneshot};

use crate::bluetooth::telephony::TelephonyJob;
use crate::config::DaemonConfig;
use crate::discovery::Advertiser;
use crate::ipc::server::DaemonEvent;
use crate::peer::identity::IdentityError;
use crate::peer::media_coordinator::MediaCoordinator;
use crate::peer::wakeup::Wakeup;
use crate::peer::PeerCommand;
use crate::state::LocalControls;
use event_loop::PeerLoop;

const THREAD_NAME: &str = "crownconnect-peer";

/// What the runtime reports to and acts through.
#[derive(Debug)]
pub struct PeerServices {
    pub daemon_events: mpsc::Sender<DaemonEvent>,
    pub controls: LocalControls,
    pub advertiser: Advertiser,
    pub telephony: mpsc::Sender<TelephonyJob>,
    pub media: Box<dyn MediaCoordinator>,
}

#[derive(Debug, thiserror::Error)]
pub enum PeerRuntimeError {
    #[error("llts transport: {0}")]
    Transport(#[from] llts_transport::TransportError),
    #[error("llts node: {0}")]
    Node(#[from] llts_node::NodeError),
    #[error("device identity: {0}")]
    Identity(#[from] IdentityError),
    #[error("peer runtime: {0}")]
    Io(#[from] std::io::Error),
    #[error("the peer runtime thread ended without reporting why")]
    Vanished,
}

/// Runs the peer runtime until `commands` closes, or until the IPC server stops listening.
///
/// # Errors
///
/// Fails when the identity or trust store cannot be loaded, or the llts socket cannot be
/// bound or polled.
pub async fn run(
    config: DaemonConfig,
    mut commands: mpsc::Receiver<PeerCommand>,
    services: PeerServices,
) -> Result<(), PeerRuntimeError> {
    let (mut thread, mut exit) = RuntimeThread::spawn(config, services)?;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => {
                    if !thread.send(command) {
                        break;
                    }
                }
                None => break,
            },
            outcome = &mut exit => return outcome.unwrap_or(Err(PeerRuntimeError::Vanished)),
        }
    }
    thread.stop();
    exit.await.unwrap_or(Err(PeerRuntimeError::Vanished))
}

type Exit = oneshot::Receiver<Result<(), PeerRuntimeError>>;

/// The runtime thread and the way in. Dropping it stops the thread and waits for it, so the
/// llts port is free again once the runtime is gone.
#[derive(Debug)]
struct RuntimeThread {
    commands: Option<std_mpsc::Sender<PeerCommand>>,
    wakeup: Arc<Wakeup>,
    handle: Option<JoinHandle<()>>,
}

impl RuntimeThread {
    fn spawn(config: DaemonConfig, services: PeerServices) -> std::io::Result<(Self, Exit)> {
        let wakeup = Arc::new(Wakeup::new()?);
        let (commands, received) = std_mpsc::channel();
        let (report, exit) = oneshot::channel();
        let thread_wakeup = Arc::clone(&wakeup);
        let handle = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || {
                let outcome = PeerLoop::open(&config, services, received, thread_wakeup)
                    .and_then(PeerLoop::run);
                let _ = report.send(outcome);
            })?;
        Ok((
            Self {
                commands: Some(commands),
                wakeup,
                handle: Some(handle),
            },
            exit,
        ))
    }

    /// `false` once the thread has stopped taking commands.
    fn send(&self, command: PeerCommand) -> bool {
        let sent = self
            .commands
            .as_ref()
            .is_some_and(|commands| commands.send(command).is_ok());
        self.wakeup.notify();
        sent
    }

    fn stop(&mut self) {
        self.commands = None;
        self.wakeup.notify();
    }
}

impl Drop for RuntimeThread {
    fn drop(&mut self) {
        self.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
