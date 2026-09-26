//! Finding paired and pairing devices nearby over BLE and mDNS, both carrying the same private
//! llts-signaling beacons, and advertising this computer's own.
//!
//! Beacons are opaque here: resolving a linked device's rotating identifier needs the trust
//! store, so it happens in the peer runtime on the [`PresenceEvent`]s both planes produce.

pub mod ble;
pub mod mdns;
mod presence;

use llts_signaling::discovery::Beacon;
use tokio::sync::{mpsc, watch};

pub use presence::{AddressHint, PresenceEvent, PresenceKey};

const PRESENCE_QUEUE_DEPTH: usize = 64;

/// Chooses what both planes advertise; an empty list advertises nothing.
#[derive(Debug)]
pub struct Advertiser(watch::Sender<Vec<Beacon>>);

impl Advertiser {
    pub fn advertise(&self, beacons: Vec<Beacon>) {
        self.0.send_replace(beacons);
    }
}

/// The advertiser, one beacon receiver per plane, and the shared presence channel.
#[derive(Debug)]
pub struct DiscoveryChannels {
    pub advertiser: Advertiser,
    pub ble_beacons: watch::Receiver<Vec<Beacon>>,
    pub mdns_beacons: watch::Receiver<Vec<Beacon>>,
    pub presence: mpsc::Sender<PresenceEvent>,
    pub presence_events: mpsc::Receiver<PresenceEvent>,
}

pub fn channels() -> DiscoveryChannels {
    let (advertiser, ble_beacons) = watch::channel(Vec::new());
    let mdns_beacons = advertiser.subscribe();
    let (presence, presence_events) = mpsc::channel(PRESENCE_QUEUE_DEPTH);
    DiscoveryChannels {
        advertiser: Advertiser(advertiser),
        ble_beacons,
        mdns_beacons,
        presence,
        presence_events,
    }
}

fn beacon_bytes(beacon: &Beacon) -> Vec<u8> {
    match beacon {
        Beacon::Linked(advertisement) => advertisement.to_bytes().to_vec(),
        Beacon::Pairing(pairing) => pairing.to_bytes().to_vec(),
    }
}
