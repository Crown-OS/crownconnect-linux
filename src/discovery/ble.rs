//! The BLE plane: llts beacons as service data under the 16-bit UUID 0xFCF2.

use std::collections::{BTreeMap, HashMap};
use std::pin::pin;
use std::time::{Duration, Instant};

use bluer::adv::{Advertisement, AdvertisementHandle, Type};
use bluer::{AdapterEvent, DiscoveryFilter, DiscoveryTransport, Uuid, UuidExt};
use futures_util::StreamExt;
use llts_signaling::discovery::{Beacon, SERVICE_UUID_16};
use tokio::sync::{mpsc, watch};

use super::beacon_bytes;
use super::presence::{AddressHint, PresenceEvent, PresenceKey, PresenceTracker};

/// BLE has no reliable "gone" signal: BlueZ keeps devices long after they leave, so a beacon not
/// heard for this long counts as lost.
const SIGHTING_LIFETIME: Duration = Duration::from_secs(30);
const EXPIRY_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum BleError {
    #[error("bluetooth: {0}")]
    Bluetooth(#[from] bluer::Error),
}

fn service_uuid() -> Uuid {
    Uuid::from_u16(SERVICE_UUID_16)
}

/// Advertises the beacons `advertised` holds and reports llts beacons heard nearby, until
/// either channel closes.
///
/// # Errors
///
/// Fails without BlueZ or a powered adapter.
pub async fn run(
    mut advertised: watch::Receiver<Vec<Beacon>>,
    presence: mpsc::Sender<PresenceEvent>,
) -> Result<(), BleError> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter
        .set_discovery_filter(DiscoveryFilter {
            uuids: [service_uuid()].into(),
            transport: DiscoveryTransport::Le,
            duplicate_data: true,
            ..DiscoveryFilter::default()
        })
        .await?;
    let mut events = pin!(adapter.discover_devices_with_changes().await?);
    let mut advertising = advertise(&adapter, &advertised.borrow_and_update()).await;
    let mut tracker = PresenceTracker::default();
    let mut expiry = tokio::time::interval(EXPIRY_INTERVAL);
    loop {
        let reports = tokio::select! {
            Some(event) = events.next() => observe(&adapter, &mut tracker, event).await,
            _ = expiry.tick() => tracker.expire(Instant::now(), SIGHTING_LIFETIME),
            changed = advertised.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                drop(std::mem::take(&mut advertising));
                advertising = advertise(&adapter, &advertised.borrow_and_update()).await;
                Vec::new()
            }
            else => return Ok(()),
        };
        for report in reports {
            if presence.send(report).await.is_err() {
                return Ok(());
            }
        }
    }
}

async fn observe(
    adapter: &bluer::Adapter,
    tracker: &mut PresenceTracker,
    event: AdapterEvent,
) -> Vec<PresenceEvent> {
    match event {
        AdapterEvent::DeviceAdded(address) => {
            let Ok(device) = adapter.device(address) else {
                return Vec::new();
            };
            let service_data = device.service_data().await.ok().flatten();
            service_data
                .as_ref()
                .and_then(beacon_in)
                .and_then(|beacon| {
                    tracker.observe(
                        PresenceKey::Ble(address),
                        beacon,
                        AddressHint::Ble(address),
                        Instant::now(),
                    )
                })
                .into_iter()
                .collect()
        }
        AdapterEvent::DeviceRemoved(address) => tracker
            .forget(&PresenceKey::Ble(address))
            .into_iter()
            .collect(),
        AdapterEvent::PropertyChanged(_) => Vec::new(),
    }
}

fn beacon_in(service_data: &HashMap<Uuid, Vec<u8>>) -> Option<Beacon> {
    Beacon::parse(service_data.get(&service_uuid())?).ok()
}

fn advertisement(beacon: &Beacon) -> Advertisement {
    Advertisement {
        advertisement_type: Type::Broadcast,
        service_data: BTreeMap::from([(service_uuid(), beacon_bytes(beacon))]),
        discoverable: Some(false),
        ..Advertisement::default()
    }
}

async fn advertise(adapter: &bluer::Adapter, beacons: &[Beacon]) -> Vec<AdvertisementHandle> {
    let mut handles = Vec::with_capacity(beacons.len());
    for beacon in beacons {
        match adapter.advertise(advertisement(beacon)).await {
            Ok(handle) => handles.push(handle),
            Err(error) => tracing::warn!(%error, "cannot advertise a beacon over BLE"),
        }
    }
    handles
}

#[cfg(test)]
mod tests {
    use llts_signaling::device::DeviceClass;
    use llts_signaling::discovery::{PairingBeacon, PairingFlags, PairingNonce};

    use super::*;

    fn pairing() -> Beacon {
        Beacon::Pairing(PairingBeacon {
            device_class: DeviceClass::Computer,
            flags: PairingFlags::NEARBY_CODE | PairingFlags::QR,
            nonce: PairingNonce([4; 8]),
        })
    }

    #[test]
    fn the_service_uuid_is_the_bluetooth_base_form_of_0xfcf2() {
        assert_eq!(
            service_uuid().to_string(),
            "0000fcf2-0000-1000-8000-00805f9b34fb"
        );
    }

    #[test]
    fn an_advertised_beacon_is_found_in_the_scanned_service_data() {
        let advertised = advertisement(&pairing());
        let scanned: HashMap<Uuid, Vec<u8>> = advertised.service_data.into_iter().collect();
        assert_eq!(beacon_in(&scanned), Some(pairing()));
    }

    #[test]
    fn foreign_service_data_is_ignored() {
        let foreign = HashMap::from([(Uuid::from_u16(0xfcf1), vec![1, 2, 3])]);
        assert_eq!(beacon_in(&foreign), None);
        let garbage = HashMap::from([(service_uuid(), vec![9; 3])]);
        assert_eq!(beacon_in(&garbage), None);
    }
}
