//! Two complete peer runtimes with their IPC servers on loopback. They pair by QR, sync state, relay commands, keep feature choices across a restart that
//! reconnects over IK, and forget each other.

#![cfg(feature = "daemon")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test that fails a precondition should fail loudly"
)]

mod support;

use std::sync::mpsc as std_mpsc;

use crownconnect_linux::ipc::proto::{
    self, CameraLens, CameraOptions, Feature, FeatureState, Topic, VideoLimits,
};
use crownconnect_linux::state::{ClipboardSnapshot, LocalState, MediaSnapshot};
use llts_node::{MediaRequest, MediaRole};
use llts_signaling::message::Media;
use llts_signaling::state::{Battery, HybridTimestamp};
use support::{is_private, pair_by_qr, received, wait_until, Daemon, Home, REPLY};

/// A daemon whose media requests land in a channel the test reads.
fn daemon(home: &Home) -> (Daemon, std_mpsc::Receiver<MediaRequest>) {
    let (media, requests) = std_mpsc::channel();
    (Daemon::start(home, Box::new(media)), requests)
}

#[test]
fn two_daemons_pair_sync_restart_and_forget() {
    let (alpha_home, beta_home) = Home::pair("alpha", "beta");
    let (mut alpha, _alpha_media) = daemon(&alpha_home);
    let (mut beta, beta_media) = daemon(&beta_home);

    let (beta_id, alpha_id) = pair_by_qr(&mut alpha, &mut beta);
    assert_ne!(alpha_id, beta_id);
    let config = alpha_home.config();
    assert!(is_private(&config.paths.identity));
    assert!(is_private(&config.paths.trust_store));
    wait_until("the ffsp link record", || {
        is_private(&config.paths.ffsp_links).then_some(())
    });

    beta.publish(LocalState::Battery(Battery {
        percent: 42,
        charging: true,
        time_to_empty_min: None,
    }));
    alpha.next_event("beta's battery", |event| {
        matches!(event, proto::Event::BatteryChanged(changed)
            if changed.id == beta_id && changed.percent == 42 && changed.charging)
    });
    let pending = alpha.client.state(beta_id, Topic::Battery).expect("state");
    assert!(alpha
        .client
        .wait(pending, REPLY)
        .expect("state reply")
        .is_some());

    beta.publish(LocalState::Clipboard(ClipboardSnapshot {
        mime: "text/plain".to_owned(),
        bytes: b"copied on beta".to_vec(),
        stamp: HybridTimestamp::default(),
    }));
    let clipboard = received("beta's clipboard", &mut alpha.controls.clipboard);
    assert_eq!(clipboard.bytes, b"copied on beta");

    let pending = alpha
        .client
        .set_feature(beta_id, Feature::Hotspot, false)
        .expect("set_feature");
    alpha
        .client
        .wait(pending, REPLY)
        .expect("set_feature reply");
    alpha.next_event("the hotspot choice", |event| {
        matches!(event, proto::Event::FeatureChanged(changed)
            if changed.feature == Feature::Hotspot && changed.state == FeatureState::Disabled)
    });
    let features_of_beta = |alpha: &mut Daemon| {
        let pending = alpha.client.features(beta_id).expect("features");
        alpha.client.wait(pending, REPLY).expect("features reply")
    };
    assert!(!features_of_beta(&mut alpha).contains(Feature::Hotspot));
    assert!(features_of_beta(&mut alpha).contains(Feature::Battery));

    beta.publish(LocalState::Media(Some(MediaSnapshot {
        app: "Player".to_owned(),
        title: "Song".to_owned(),
        artist: "Band".to_owned(),
        position_ms: 1_000,
        duration_ms: 180_000,
        playing: false,
    })));
    alpha.next_event("beta's player", |event| {
        matches!(event, proto::Event::MediaChanged(changed)
            if changed.id == beta_id && changed.media.is_some())
    });
    alpha.client.media_play().expect("media_play");
    assert_eq!(
        received("the play command on beta", &mut beta.controls.media),
        Media::Play
    );

    let camera = CameraOptions {
        lens: CameraLens::Front,
        limits: VideoLimits {
            max_width: 1280,
            max_height: 720,
            max_fps: 30,
        },
    };
    let pending = alpha
        .client
        .start_camera(beta_id, camera)
        .expect("start_camera");
    alpha.client.wait(pending, REPLY).expect("camera accepted");
    alpha.next_event("the camera running", |event| {
        matches!(event, proto::Event::FeatureChanged(changed)
            if changed.feature == Feature::Camera && changed.state == FeatureState::Active)
    });
    let encoder = wait_until("beta's camera encoder", || beta_media.try_recv().ok());
    assert!(matches!(
        encoder,
        MediaRequest::Start {
            role: MediaRole::Encoder,
            ..
        }
    ));
    let pending = alpha.client.stop(beta_id, Feature::Camera).expect("stop");
    alpha.client.wait(pending, REPLY).expect("camera stopped");

    alpha.stop();
    let (mut alpha, _alpha_media) = daemon(&alpha_home);
    assert_eq!(alpha.connected_peer(), beta_id);
    assert!(!features_of_beta(&mut alpha).contains(Feature::Hotspot));

    let pending = alpha.client.forget(beta_id).expect("forget");
    alpha.client.wait(pending, REPLY).expect("forgotten");
    wait_until("alpha to forget beta", || {
        alpha.devices().is_empty().then_some(())
    });
    wait_until("beta to forget alpha", || {
        beta.devices().is_empty().then_some(())
    });

    alpha.stop();
    beta.stop();
}
