#![cfg(feature = "daemon")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test that fails a precondition should fail loudly"
)]

//! Read-only checks against the platform services of the machine running the tests. A service
//! that is missing skips its test with a message instead of failing it.

use std::future::Future;
use std::time::Duration;

use crownconnect_linux::state::publishers::{battery, hotspot, media, volume};
use crownconnect_linux::state::{state_channel, LocalState, StateSink};
use tokio::sync::mpsc;

const FIRST_VALUE: Duration = Duration::from_secs(3);

/// The first value `publisher` sends, or `None` when it fails or stays silent.
fn first_value<E, F>(name: &str, publisher: impl FnOnce(StateSink) -> F) -> Option<LocalState>
where
    E: std::fmt::Display,
    F: Future<Output = Result<(), E>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (sink, mut values) = state_channel();
    runtime.block_on(async {
        let run = publisher(sink);
        tokio::select! {
            outcome = run => {
                match outcome {
                    Ok(()) => eprintln!("skipping {name}: the publisher stopped immediately"),
                    Err(error) => eprintln!("skipping {name}: {error}"),
                }
                None
            }
            value = tokio::time::timeout(FIRST_VALUE, values.recv()) => {
                let value = value.ok().flatten();
                if value.is_none() {
                    eprintln!("skipping {name}: nothing published within {FIRST_VALUE:?}");
                }
                value
            }
        }
    })
}

#[test]
fn upower_reports_a_plausible_battery() {
    if let Some(state) = first_value("battery", battery::run) {
        let LocalState::Battery(battery) = state else {
            panic!("battery published {state:?}");
        };
        assert!(battery.percent <= 100);
    }
}

#[test]
fn mpris_reports_the_active_player_or_none() {
    let (_commands, receiver) = mpsc::channel(1);
    if let Some(state) = first_value("media", |sink| media::run(sink, receiver)) {
        assert!(
            matches!(state, LocalState::Media(_)),
            "media published {state:?}"
        );
    }
}

#[test]
fn pipewire_reports_the_default_sink_volume() {
    let (_commands, receiver) = mpsc::channel(1);
    if let Some(state) = first_value("volume", |sink| volume::run(sink, receiver)) {
        let LocalState::Volume(volume) = state else {
            panic!("volume published {state:?}");
        };
        assert!(volume.percent <= 100);
    }
}

#[test]
fn network_manager_reports_the_hotspot() {
    let (_commands, receiver) = mpsc::channel(1);
    if let Some(state) = first_value("hotspot", |sink| hotspot::run(sink, receiver)) {
        let LocalState::Hotspot(hotspot) = state else {
            panic!("hotspot published {state:?}");
        };
        assert_eq!(hotspot.enabled, hotspot.ssid.is_some());
    }
}
