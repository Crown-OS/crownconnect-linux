//! Complete daemons in-process on loopback, each with its own data directory, socket directory
//! and llts port, found through static peers instead of BLE/mDNS.

#![allow(
    dead_code,
    unreachable_pub,
    reason = "each test binary uses its own part of the harness"
)]

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crownconnect_linux::config::{default_features, DaemonConfig, DaemonPaths};
use crownconnect_linux::discovery;
use crownconnect_linux::ipc::proto::{self, DeviceId, DeviceInfo};
use crownconnect_linux::ipc::server::IpcServer;
use crownconnect_linux::peer::{runtime, MediaCoordinator, PeerCommand, PeerServices};
use crownconnect_linux::state::{LocalCommandReceivers, LocalControls, LocalState};
use tokio::sync::{mpsc, oneshot};

pub const REPLY: Option<Duration> = Some(Duration::from_secs(10));
pub const PATIENCE: Duration = Duration::from_secs(20);
pub const POLL: Duration = Duration::from_millis(10);

/// Logs the daemons' tracing to stderr when `CROWNCONNECT_TEST_LOG` holds a filter.
pub fn init_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let Some(filter) = std::env::var("CROWNCONNECT_TEST_LOG")
        .ok()
        .and_then(|filter| filter.parse::<tracing_subscriber::filter::Targets>().ok())
    else {
        return;
    };
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(filter)
        .try_init();
}

pub fn free_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|socket| socket.local_addr())
        .expect("a free port")
        .port()
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// One daemon's files and addresses, kept across a restart.
pub struct Home {
    name: &'static str,
    root: PathBuf,
    port: u16,
    peer_port: u16,
}

impl Home {
    pub fn new(name: &'static str, port: u16, peer_port: u16) -> Self {
        let root = std::env::temp_dir().join(format!(
            "crownconnect-daemons-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Self {
            name,
            root,
            port,
            peer_port,
        }
    }

    /// Two homes that know each other's port.
    pub fn pair(first: &'static str, second: &'static str) -> (Self, Self) {
        let (first_port, second_port) = (free_port(), free_port());
        (
            Self::new(first, first_port, second_port),
            Self::new(second, second_port, first_port),
        )
    }

    fn data_home(&self) -> PathBuf {
        self.root.join("data")
    }

    fn ipc_directory(&self) -> PathBuf {
        self.root.join("ipc")
    }

    pub fn config(&self) -> DaemonConfig {
        DaemonConfig {
            paths: DaemonPaths::in_directories(
                self.data_home().join("crownos/crownconnect"),
                &self.data_home(),
                Some(self.ipc_directory()),
            ),
            llts_port: self.port,
            llts_bind: Ipv4Addr::LOCALHOST.into(),
            static_peers: vec![loopback(self.peer_port)],
            device_name: self.name.to_owned(),
            default_features: default_features(),
            camera_device: None,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A running daemon: its IPC server and peer runtime on their own thread, with fake local
/// controls the test reads and the media coordinator it was given.
pub struct Daemon {
    pub peer: mpsc::Sender<PeerCommand>,
    pub controls: LocalCommandReceivers,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    pub client: proto::Client,
}

impl Daemon {
    pub fn start(home: &Home, media: Box<dyn MediaCoordinator>) -> Self {
        let config = home.config();
        let ipc_directory = home.ipc_directory();
        let (controls, receivers) = LocalControls::channels();
        let (started, ready) = std_mpsc::channel();
        let (shutdown, stop) = oneshot::channel::<()>();
        let directory = ipc_directory.clone();
        let thread = std::thread::spawn(move || {
            let tokio = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio");
            tokio.block_on(async move {
                let (peer, commands) = mpsc::channel(64);
                let (daemon_events, events) = mpsc::channel(256);
                let server = IpcServer::bind(Some(&directory), peer.clone(), events).expect("bind");
                let (telephony, _telephony_jobs) = mpsc::channel(1);
                let services = PeerServices {
                    daemon_events,
                    controls,
                    advertiser: discovery::channels().advertiser,
                    telephony,
                    media,
                };
                started.send(peer).expect("started");
                tokio::select! {
                    served = server.run() => served.expect("ipc server"),
                    ran = runtime::run(config, commands, services) => ran.expect("peer runtime"),
                    _ = stop => {}
                }
            });
        });
        let peer = ready.recv().expect("the daemon started");
        let mut client = proto::Client::connect_in(&ipc_directory).expect("connect");
        client
            .subscribe_blocking::<proto::BatteryChanged>(REPLY)
            .expect("subscribe battery");
        client
            .subscribe_blocking::<proto::MediaChanged>(REPLY)
            .expect("subscribe media");
        client
            .subscribe_blocking::<proto::FeatureChanged>(REPLY)
            .expect("subscribe features");
        client
            .subscribe_blocking::<proto::FeatureFailed>(REPLY)
            .expect("subscribe feature failures");
        Self {
            peer,
            controls: receivers,
            shutdown: Some(shutdown),
            thread: Some(thread),
            client,
        }
    }

    pub fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().expect("daemon thread");
        }
    }

    pub fn publish(&self, state: LocalState) {
        self.peer
            .blocking_send(PeerCommand::LocalState(state))
            .expect("the runtime takes local state");
    }

    pub fn devices(&mut self) -> Vec<DeviceInfo> {
        let pending = self.client.devices().expect("devices");
        self.client.wait(pending, REPLY).expect("devices reply")
    }

    /// The one device this daemon knows, once it is connected.
    pub fn connected_peer(&mut self) -> DeviceId {
        wait_until("the peer to connect", || {
            let devices = self.devices();
            match devices.as_slice() {
                [device] if device.connected => Some(device.id),
                _ => None,
            }
        })
    }

    pub fn next_event(
        &mut self,
        what: &str,
        matches: impl Fn(&proto::Event) -> bool,
    ) -> proto::Event {
        let deadline = Instant::now() + PATIENCE;
        loop {
            while let Some(event) = self.client.next_event() {
                if matches(&event) {
                    return event;
                }
            }
            assert!(Instant::now() < deadline, "{what} never arrived");
            let _ = self.client.handle_readable();
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Pairs `second` with `first` by QR and waits until both see the other connected; returns the
/// ids of `second` as `first` sees it and of `first` as `second` sees it.
pub fn pair_by_qr(first: &mut Daemon, second: &mut Daemon) -> (DeviceId, DeviceId) {
    let pending = first.client.pairing_begin().expect("pairing_begin");
    let offer = first.client.wait(pending, REPLY).expect("a pairing offer");
    assert!(offer.qr.starts_with("llts1:"));
    let pending = second.client.pair_with_qr(offer.qr).expect("pair_with_qr");
    second.client.wait(pending, REPLY).expect("paired by QR");
    (first.connected_peer(), second.connected_peer())
}

pub fn wait_until<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(value) = ready() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(POLL);
    }
}

pub fn received<T>(what: &str, receiver: &mut mpsc::Receiver<T>) -> T {
    wait_until(what, || receiver.try_recv().ok())
}

pub fn is_private(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o777 == 0o600)
}
