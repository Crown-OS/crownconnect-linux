//! The supervisor: wires publishers, discovery and telephony into the peer runtime, serves IPC,
//! and shuts everything down on Ctrl-C.

use std::future::Future;

use anyhow::{anyhow, Context, Result};
use crownconnect_linux::bluetooth::telephony;
use crownconnect_linux::config::DaemonConfig;
use crownconnect_linux::discovery::{self, ble, mdns};
use crownconnect_linux::ipc::server::IpcServer;
use std::sync::Arc;

use crownconnect_linux::features::{FeatureCoordinator, LinuxPlatform};
use crownconnect_linux::peer::{runtime, PeerCommand, PeerServices};
use crownconnect_linux::state::publishers::{battery, clipboard, hotspot, media, volume};
use crownconnect_linux::state::{session_lock, state_channel, LocalControls};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

const PEER_QUEUE_DEPTH: usize = 64;
/// The runtime thread waits when this fills, so it is sized for a burst of state updates.
const DAEMON_EVENT_QUEUE_DEPTH: usize = 256;
const CALLS_QUEUE_DEPTH: usize = 8;
const TELEPHONY_QUEUE_DEPTH: usize = 8;

/// Whether the daemon can run without a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Critical,
    /// A platform service this computer may lack, such as a battery or a Bluetooth adapter.
    Optional,
}

type Finished = (&'static str, Role, Result<()>);

fn spawn<E>(
    tasks: &mut JoinSet<Finished>,
    name: &'static str,
    role: Role,
    work: impl Future<Output = Result<(), E>> + 'static,
) where
    E: std::error::Error + Send + Sync + 'static,
{
    tasks.spawn_local(async move { (name, role, work.await.map_err(anyhow::Error::from)) });
}

fn forward<T: 'static>(
    tasks: &mut JoinSet<Finished>,
    name: &'static str,
    mut from: mpsc::Receiver<T>,
    to: mpsc::Sender<PeerCommand>,
    wrap: fn(T) -> PeerCommand,
) {
    tasks.spawn_local(async move {
        while let Some(item) = from.recv().await {
            if to.send(wrap(item)).await.is_err() {
                break;
            }
        }
        (name, Role::Optional, Ok(()))
    });
}

/// Runs the daemon until Ctrl-C or until a critical task fails.
///
/// # Errors
///
/// Fails when the IPC socket cannot be bound or a critical task stops.
pub(crate) async fn run(config: DaemonConfig) -> Result<()> {
    tokio::task::LocalSet::new()
        .run_until(supervise(config))
        .await
}

async fn supervise(config: DaemonConfig) -> Result<()> {
    let (peer, peer_commands) = mpsc::channel(PEER_QUEUE_DEPTH);
    let (daemon_events, daemon_event_receiver) = mpsc::channel(DAEMON_EVENT_QUEUE_DEPTH);
    let server = IpcServer::bind(
        config.paths.ipc_directory.as_deref(),
        peer.clone(),
        daemon_event_receiver,
    )
    .context("cannot bind the crownconnect IPC service")?;
    let (sink, local_states) = state_channel();
    let (controls, commands) = LocalControls::channels();
    let discovery = discovery::channels();
    let (calls, phone_calls) = mpsc::channel(CALLS_QUEUE_DEPTH);
    let (telephony, telephony_jobs) = mpsc::channel(TELEPHONY_QUEUE_DEPTH);
    let services = PeerServices {
        daemon_events,
        controls,
        advertiser: discovery.advertiser,
        telephony,
        media: Box::new(FeatureCoordinator::new(Arc::new(LinuxPlatform::new(
            &config,
        )))),
    };

    let mut tasks = JoinSet::new();
    spawn(&mut tasks, "ipc", Role::Critical, server.run());
    spawn(
        &mut tasks,
        "peer runtime",
        Role::Critical,
        runtime::run(config.clone(), peer_commands, services),
    );
    spawn(
        &mut tasks,
        "battery",
        Role::Optional,
        battery::run(sink.clone()),
    );
    spawn(
        &mut tasks,
        "media",
        Role::Optional,
        media::run(sink.clone(), commands.media),
    );
    spawn(
        &mut tasks,
        "volume",
        Role::Optional,
        volume::run(sink.clone(), commands.volume),
    );
    spawn(
        &mut tasks,
        "hotspot",
        Role::Optional,
        hotspot::run(sink.clone(), commands.hotspot),
    );
    spawn(
        &mut tasks,
        "clipboard",
        Role::Optional,
        clipboard::run(sink, commands.clipboard),
    );
    spawn(
        &mut tasks,
        "session lock",
        Role::Optional,
        session_lock::run(commands.lock),
    );
    spawn(
        &mut tasks,
        "ble",
        Role::Optional,
        ble::run(discovery.ble_beacons, discovery.presence.clone()),
    );
    spawn(
        &mut tasks,
        "mdns",
        Role::Optional,
        mdns::run(discovery.mdns_beacons, config.llts_port, discovery.presence),
    );
    spawn(
        &mut tasks,
        "telephony",
        Role::Optional,
        telephony::watch(calls),
    );
    spawn(
        &mut tasks,
        "telephony control",
        Role::Optional,
        telephony::serve(telephony_jobs),
    );
    forward(
        &mut tasks,
        "local state",
        local_states,
        peer.clone(),
        PeerCommand::LocalState,
    );
    forward(
        &mut tasks,
        "presence",
        discovery.presence_events,
        peer.clone(),
        PeerCommand::Presence,
    );
    forward(
        &mut tasks,
        "phone calls",
        phone_calls,
        peer,
        PeerCommand::Calls,
    );

    let outcome = loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                break signal.context("cannot listen for Ctrl-C");
            }
            Some(finished) = tasks.join_next() => match finished {
                Ok((name, Role::Critical, result)) => {
                    break result.and(Err(anyhow!("stopped"))).with_context(|| format!("{name} task"));
                }
                Ok((name, Role::Optional, Err(error))) => {
                    tracing::warn!(service = name, error = %error, "unavailable");
                }
                Ok((name, Role::Optional, Ok(()))) => tracing::debug!(service = name, "stopped"),
                Err(error) => break Err(error).context("a daemon task panicked"),
            },
        }
    };
    tracing::info!("shutting down");
    tasks.shutdown().await;
    outcome
}
