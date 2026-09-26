#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test that fails a precondition should fail loudly"
)]

use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crownconnect_linux::ipc::proto::{
    self, Battery, CameraOptions, DeviceClass, DeviceId, DeviceInfo, Edge, Feature, FeatureSet,
    FeatureState, LinkKind, MirrorOptions, MonitorOptions, PairingOffer, Topic,
};
use crownos_ipc::{blocking, Error, MethodMsg, RemoteError, ServiceBuilder};

const TIMEOUT: Option<Duration> = Some(Duration::from_secs(5));
const PHONE: DeviceId = DeviceId([7; 32]);
const STRANGER: DeviceId = DeviceId([9; 32]);

#[derive(Debug, Default)]
struct FakeHub {
    features: FeatureSet,
    last_seek: Option<u8>,
    picked_up: Vec<String>,
}

fn phone() -> DeviceInfo {
    DeviceInfo {
        id: PHONE,
        name: "Pixel".into(),
        class: DeviceClass::Phone,
        trusted: true,
        connected: true,
        link: LinkKind::Lan,
        battery: Some(Battery {
            percent: 80,
            charging: false,
        }),
        features: FeatureSet::EMPTY.with(Feature::Clipboard),
        active: FeatureSet::EMPTY,
    }
}

fn known(id: DeviceId) -> Result<(), RemoteError> {
    if id == PHONE {
        Ok(())
    } else {
        Err(RemoteError::handler(format!("unknown device {id}")))
    }
}

impl proto::Handler for FakeHub {
    fn devices(&mut self) -> Result<Vec<DeviceInfo>, RemoteError> {
        Ok(vec![phone()])
    }

    fn pairing_begin(&mut self) -> Result<PairingOffer, RemoteError> {
        Ok(PairingOffer {
            qr: "crownconnect:offer".into(),
            expires_unix_ms: 1,
        })
    }

    fn pairing_confirm(&mut self, id: DeviceId, _accept: bool) -> Result<(), RemoteError> {
        known(id)
    }

    fn pairing_cancel(&mut self) {}

    fn forget(&mut self, id: DeviceId) -> Result<(), RemoteError> {
        known(id)
    }

    fn set_feature(
        &mut self,
        id: DeviceId,
        feature: Feature,
        enabled: bool,
    ) -> Result<(), RemoteError> {
        known(id)?;
        self.features = if enabled {
            self.features.with(feature)
        } else {
            self.features.without(feature)
        };
        Ok(())
    }

    fn features(&mut self, id: DeviceId) -> Result<FeatureSet, RemoteError> {
        known(id).map(|()| self.features)
    }

    fn send_file(&mut self, id: DeviceId, _path: String) -> Result<(), RemoteError> {
        known(id)
    }

    fn start_mirror(&mut self, id: DeviceId, _options: MirrorOptions) -> Result<(), RemoteError> {
        known(id)
    }

    fn start_camera(&mut self, id: DeviceId, _options: CameraOptions) -> Result<(), RemoteError> {
        known(id)
    }

    fn start_mic(&mut self, id: DeviceId) -> Result<(), RemoteError> {
        known(id)
    }

    fn start_monitor(&mut self, id: DeviceId, _options: MonitorOptions) -> Result<(), RemoteError> {
        known(id)
    }

    fn stop(&mut self, id: DeviceId, _feature: Feature) -> Result<(), RemoteError> {
        known(id)
    }

    fn set_unicursor_edge(&mut self, id: DeviceId, _edge: Edge) -> Result<(), RemoteError> {
        known(id)
    }

    fn pickup_call(&mut self, call_id: String) -> Result<(), RemoteError> {
        self.picked_up.push(call_id);
        Ok(())
    }

    fn decline_call(&mut self, _call_id: String) -> Result<(), RemoteError> {
        Ok(())
    }

    fn hangup_call(&mut self, _call_id: String) -> Result<(), RemoteError> {
        Ok(())
    }

    fn dial(&mut self, id: DeviceId, _number: String) -> Result<(), RemoteError> {
        known(id)
    }

    fn media_play(&mut self) {}

    fn media_pause(&mut self) {}

    fn media_next(&mut self) {}

    fn media_previous(&mut self) {}

    fn media_seek(&mut self, percent: u8) {
        self.last_seek = Some(percent);
    }

    fn send_reply(&mut self, _conversation_id: String, _text: String) {}

    fn state(&mut self, id: DeviceId, topic: Topic) -> Result<Option<Vec<u8>>, RemoteError> {
        known(id).map(|()| (topic == Topic::Battery).then(|| vec![80, 0]))
    }

    fn set_hotspot(&mut self, id: DeviceId, _enabled: bool) -> Result<(), RemoteError> {
        known(id)
    }

    fn pair_with_qr(&mut self, qr: String) -> Result<(), RemoteError> {
        if qr.is_empty() {
            Err(RemoteError::handler("empty invitation"))
        } else {
            Ok(())
        }
    }
}

fn socket_directory() -> PathBuf {
    std::env::temp_dir().join(format!("crownconnect-ipc-{}", std::process::id()))
}

fn spawn_hub(directory: PathBuf) -> JoinHandle<FakeHub> {
    let ready = Arc::new(Barrier::new(2));
    let server_ready = Arc::clone(&ready);
    let handle = std::thread::spawn(move || {
        let mut server = ServiceBuilder::new(proto::SERVICE)
            .directory(&directory)
            .build()
            .expect("bind");
        server_ready.wait();
        let mut hub = FakeHub::default();
        blocking::serve(&mut server, |server, peer, message| {
            let selector = message.selector;
            proto::dispatch(&mut hub, server, peer, message)?;
            if selector == <proto::set_feature as MethodMsg>::SELECTOR {
                server.emit(&proto::FeatureChanged {
                    id: PHONE,
                    feature: Feature::Camera,
                    state: FeatureState::Enabled,
                })?;
            }
            Ok(selector != <proto::pairing_cancel as MethodMsg>::SELECTOR)
        })
        .expect("serve");
        hub
    });
    ready.wait();
    handle
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

#[test]
fn a_client_drives_the_hub_through_the_frozen_schema() {
    let directory = socket_directory();
    let hub = spawn_hub(directory.clone());
    let mut client = proto::Client::connect_in(&directory).expect("connect");

    let pending = client.devices().expect("devices");
    assert_eq!(
        client.wait(pending, TIMEOUT).expect("devices reply"),
        [phone()]
    );

    client
        .subscribe_blocking::<proto::FeatureChanged>(TIMEOUT)
        .expect("subscribe");
    let pending = client
        .set_feature(PHONE, Feature::Camera, true)
        .expect("set_feature");
    client.wait(pending, TIMEOUT).expect("set_feature reply");
    match next_event(&mut client) {
        proto::Event::FeatureChanged(changed) => {
            assert_eq!(changed.feature, Feature::Camera);
            assert_eq!(changed.state, FeatureState::Enabled);
        }
        other => panic!("unexpected event {other:?}"),
    }

    let pending = client.features(PHONE).expect("features");
    let features = client.wait(pending, TIMEOUT).expect("features reply");
    assert!(features.contains(Feature::Camera));

    let pending = client.state(PHONE, Topic::Battery).expect("state");
    assert_eq!(
        client.wait(pending, TIMEOUT).expect("state reply"),
        Some(vec![80, 0])
    );

    let pending = client.forget(STRANGER).expect("forget");
    match client.wait(pending, TIMEOUT) {
        Err(Error::Remote(remote)) => assert_eq!(remote.code, RemoteError::HANDLER),
        other => panic!("expected a handler error, got {other:?}"),
    }

    let pending = client.pickup_call("call-1".into()).expect("pickup");
    client.wait(pending, TIMEOUT).expect("pickup reply");
    client.media_seek(42).expect("seek");
    client.pairing_cancel().expect("stop");

    let hub = hub.join().expect("hub thread");
    assert_eq!(hub.last_seek, Some(42));
    assert_eq!(hub.picked_up, ["call-1"]);
}

#[test]
fn every_selector_in_the_schema_is_distinct() {
    use crownos_ipc::EventMsg;
    let mut selectors = vec![
        <proto::devices as MethodMsg>::SELECTOR,
        <proto::pairing_begin as MethodMsg>::SELECTOR,
        <proto::pairing_confirm as MethodMsg>::SELECTOR,
        <proto::pairing_cancel as MethodMsg>::SELECTOR,
        <proto::forget as MethodMsg>::SELECTOR,
        <proto::set_feature as MethodMsg>::SELECTOR,
        <proto::features as MethodMsg>::SELECTOR,
        <proto::send_file as MethodMsg>::SELECTOR,
        <proto::start_mirror as MethodMsg>::SELECTOR,
        <proto::start_camera as MethodMsg>::SELECTOR,
        <proto::start_mic as MethodMsg>::SELECTOR,
        <proto::start_monitor as MethodMsg>::SELECTOR,
        <proto::stop as MethodMsg>::SELECTOR,
        <proto::set_unicursor_edge as MethodMsg>::SELECTOR,
        <proto::pickup_call as MethodMsg>::SELECTOR,
        <proto::decline_call as MethodMsg>::SELECTOR,
        <proto::hangup_call as MethodMsg>::SELECTOR,
        <proto::dial as MethodMsg>::SELECTOR,
        <proto::media_play as MethodMsg>::SELECTOR,
        <proto::media_pause as MethodMsg>::SELECTOR,
        <proto::media_next as MethodMsg>::SELECTOR,
        <proto::media_previous as MethodMsg>::SELECTOR,
        <proto::media_seek as MethodMsg>::SELECTOR,
        <proto::send_reply as MethodMsg>::SELECTOR,
        <proto::state as MethodMsg>::SELECTOR,
        <proto::set_hotspot as MethodMsg>::SELECTOR,
        <proto::DeviceConnected as EventMsg>::SELECTOR,
        <proto::DevicesChanged as EventMsg>::SELECTOR,
        <proto::PairingRequest as EventMsg>::SELECTOR,
        <proto::PairingResult as EventMsg>::SELECTOR,
        <proto::CallStateChanged as EventMsg>::SELECTOR,
        <proto::MediaChanged as EventMsg>::SELECTOR,
        <proto::BatteryChanged as EventMsg>::SELECTOR,
        <proto::HotspotChanged as EventMsg>::SELECTOR,
        <proto::FeatureChanged as EventMsg>::SELECTOR,
        <proto::FeatureFailed as EventMsg>::SELECTOR,
    ];
    let declared = selectors.len();
    selectors.sort_unstable();
    selectors.dedup();
    assert_eq!(selectors.len(), declared);
}
