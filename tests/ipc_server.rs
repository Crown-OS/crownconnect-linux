#![cfg(feature = "daemon")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test that fails a precondition should fail loudly"
)]

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crownconnect_linux::ipc::proto::{
    self, Battery, DeviceClass, DeviceId, DeviceInfo, Direction, Feature, FeatureSet, LinkKind,
    MirrorOptions, PairingOffer, Topic, VideoLimits,
};
use crownconnect_linux::ipc::server::{DaemonEvent, IpcServer, OwnedUpdate};
use crownconnect_linux::peer::{PeerCommand, PeerNotice, PeerRequest};
use crownos_ipc::{Error, RemoteError};
use llts_signaling::message::Media;
use llts_signaling::state::{Battery as BatteryState, Incarnation, TopicPayload, Version};
use tokio::sync::mpsc;

const TIMEOUT: Option<Duration> = Some(Duration::from_secs(5));
const PHONE: DeviceId = DeviceId([7; 32]);
const STRANGER: DeviceId = DeviceId([9; 32]);
const NO_SESSION: &str = "no session with that device";

fn phone() -> DeviceInfo {
    DeviceInfo {
        id: PHONE,
        name: "Pixel".into(),
        class: DeviceClass::Phone,
        trusted: true,
        connected: true,
        link: LinkKind::Lan,
        battery: None,
        features: FeatureSet::EMPTY.with(Feature::Battery),
        active: FeatureSet::EMPTY,
    }
}

fn socket_directory(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("crownconnect-server-{}-{name}", std::process::id()))
}

/// Answers pairing, refuses streaming, and reports every notice it receives.
async fn fake_runtime(
    mut commands: mpsc::Receiver<PeerCommand>,
    notices: std_mpsc::Sender<PeerNotice>,
) {
    while let Some(command) = commands.recv().await {
        match command {
            PeerCommand::Request(PeerRequest::PairingBegin(reply)) => reply.reply(PairingOffer {
                qr: "crownconnect:test".into(),
                expires_unix_ms: 5,
            }),
            PeerCommand::Request(request) => request.reject(NO_SESSION),
            PeerCommand::Notice(notice) => {
                let _ = notices.send(notice);
            }
            _ => {}
        }
    }
}

enum Runtime {
    Fake(std_mpsc::Sender<PeerNotice>),
    Stopped,
}

struct Daemon {
    events: mpsc::Sender<DaemonEvent>,
    thread: JoinHandle<()>,
}

fn spawn_daemon(directory: PathBuf, runtime: Runtime) -> Daemon {
    let (ready, started) = std_mpsc::channel();
    let thread = std::thread::spawn(move || {
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        tokio.block_on(async move {
            let (peer, commands) = mpsc::channel(8);
            let (events, event_receiver) = mpsc::channel(8);
            let server = IpcServer::bind(Some(&directory), peer, event_receiver).expect("bind");
            match runtime {
                Runtime::Fake(notices) => {
                    tokio::spawn(fake_runtime(commands, notices));
                }
                Runtime::Stopped => drop(commands),
            }
            ready.send(events).expect("ready");
            server.run().await.expect("serve");
        });
    });
    let events = started.recv().expect("daemon started");
    Daemon { events, thread }
}

fn next_event(client: &mut proto::Client) -> proto::Event {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = client.next_event() {
            return event;
        }
        assert!(Instant::now() < deadline, "event never arrived");
        let _ = client.handle_readable();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn battery_update(percent: u8) -> OwnedUpdate {
    let battery = BatteryState {
        percent,
        charging: false,
        time_to_empty_min: Some(90),
    };
    OwnedUpdate {
        topic: BatteryState::TOPIC,
        version: Version {
            incarnation: Incarnation::from_raw(3),
            seq: 1,
        },
        payload: postcard::to_stdvec(&battery).expect("encode"),
    }
}

fn handler_error<T: std::fmt::Debug>(outcome: Result<T, Error>) -> RemoteError {
    match outcome {
        Err(Error::Remote(remote)) => remote,
        other => panic!("expected a remote error, got {other:?}"),
    }
}

const MIRROR: MirrorOptions = MirrorOptions {
    direction: Direction::FromPeer,
    limits: VideoLimits {
        max_width: 1920,
        max_height: 1080,
        max_fps: 60,
    },
    remote_input: true,
};

#[test]
fn the_server_caches_runtime_events_and_routes_requests_to_the_runtime() {
    let directory = socket_directory("fake");
    let (notices, notice_receiver) = std_mpsc::channel();
    let daemon = spawn_daemon(directory.clone(), Runtime::Fake(notices));
    let mut client = proto::Client::connect_in(&directory).expect("connect");
    client
        .subscribe_blocking::<proto::DevicesChanged>(TIMEOUT)
        .expect("subscribe devices");
    client
        .subscribe_blocking::<proto::BatteryChanged>(TIMEOUT)
        .expect("subscribe battery");
    client
        .subscribe_blocking::<proto::FeatureChanged>(TIMEOUT)
        .expect("subscribe features");

    daemon
        .events
        .blocking_send(DaemonEvent::DeviceUpdated(phone()))
        .expect("device");
    match next_event(&mut client) {
        proto::Event::DevicesChanged(changed) => assert_eq!(changed.devices, [phone()]),
        other => panic!("unexpected event {other:?}"),
    }

    let update = battery_update(55);
    daemon
        .events
        .blocking_send(DaemonEvent::StateUpdate {
            owner: PHONE,
            update: update.clone(),
        })
        .expect("battery");
    match next_event(&mut client) {
        proto::Event::BatteryChanged(changed) => {
            assert_eq!(
                (changed.id, changed.percent, changed.charging),
                (PHONE, 55, false)
            );
        }
        other => panic!("unexpected event {other:?}"),
    }
    let pending = client.state(PHONE, Topic::Battery).expect("state");
    assert_eq!(
        client.wait(pending, TIMEOUT).expect("state reply"),
        Some(update.payload)
    );
    let pending = client.devices().expect("devices");
    let devices = client.wait(pending, TIMEOUT).expect("devices reply");
    assert_eq!(
        devices.first().and_then(|device| device.battery),
        Some(Battery {
            percent: 55,
            charging: false
        })
    );

    let pending = client
        .set_feature(PHONE, Feature::Camera, true)
        .expect("set_feature");
    client.wait(pending, TIMEOUT).expect("set_feature reply");
    assert!(matches!(
        next_event(&mut client),
        proto::Event::FeatureChanged(_)
    ));
    let pending = client.features(PHONE).expect("features");
    assert!(client
        .wait(pending, TIMEOUT)
        .expect("features reply")
        .contains(Feature::Camera));
    let pending = client.features(STRANGER).expect("features");
    assert_eq!(
        handler_error(client.wait(pending, TIMEOUT)).code,
        RemoteError::HANDLER
    );

    let pending = client.pairing_begin().expect("pairing_begin");
    assert_eq!(
        client.wait(pending, TIMEOUT).expect("offer").qr,
        "crownconnect:test"
    );
    let pending = client.start_mirror(PHONE, MIRROR).expect("start_mirror");
    assert_eq!(
        handler_error(client.wait(pending, TIMEOUT)).message,
        NO_SESSION
    );

    client.media_seek(42).expect("seek");
    client.pairing_cancel().expect("cancel");
    let received: Vec<PeerNotice> = (0..3)
        .map(|_| {
            notice_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("notice")
        })
        .collect();
    assert!(received.contains(&PeerNotice::Media(Media::Seek { percent: 42 })));
    assert!(received.contains(&PeerNotice::PairingCancel));
    assert!(received.iter().any(|notice| matches!(
        notice,
        PeerNotice::FeaturesChanged { id, features } if *id == PHONE && features.contains(Feature::Camera)
    )));

    drop(daemon.events);
    daemon.thread.join().expect("daemon thread");
}

#[test]
fn without_a_peer_runtime_peer_requests_fail_with_a_clear_error() {
    let directory = socket_directory("stopped");
    let daemon = spawn_daemon(directory.clone(), Runtime::Stopped);
    let mut client = proto::Client::connect_in(&directory).expect("connect");

    let pending = client.pairing_begin().expect("pairing_begin");
    let error = handler_error(client.wait(pending, TIMEOUT));
    assert_eq!(error.code, RemoteError::HANDLER);
    assert!(error.message.starts_with("peer runtime unavailable"));

    let pending = client.devices().expect("devices");
    assert!(client
        .wait(pending, TIMEOUT)
        .expect("devices reply")
        .is_empty());

    drop(daemon.events);
    daemon.thread.join().expect("daemon thread");
}
