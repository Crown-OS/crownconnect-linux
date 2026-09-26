use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use llts_signaling::discovery::Beacon;

/// Identifies one sighting source, so repeated sightings update instead of duplicating.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PresenceKey {
    Ble(bluer::Address),
    /// The mDNS instance's full name.
    Mdns(String),
}

/// Where the peer behind a beacon can be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressHint {
    Ble(bluer::Address),
    Lan { addresses: Vec<IpAddr>, port: u16 },
}

/// A peer appearing or disappearing on one discovery plane. BLE and mDNS carry byte-identical
/// beacons, so a linked device seen on both resolves to the same peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenceEvent {
    Seen {
        key: PresenceKey,
        beacon: Beacon,
        hint: AddressHint,
    },
    Lost {
        key: PresenceKey,
        beacon: Beacon,
    },
}

#[derive(Debug)]
struct Sighting {
    beacon: Beacon,
    hint: AddressHint,
    last_seen: Instant,
}

/// Turns a stream of raw sightings into seen/lost transitions.
#[derive(Debug, Default)]
pub(super) struct PresenceTracker {
    sightings: HashMap<PresenceKey, Sighting>,
}

impl PresenceTracker {
    /// Reports a sighting only when it is new or its beacon or address changed.
    pub(super) fn observe(
        &mut self,
        key: PresenceKey,
        beacon: Beacon,
        hint: AddressHint,
        now: Instant,
    ) -> Option<PresenceEvent> {
        if let Some(known) = self.sightings.get_mut(&key)
            && known.beacon == beacon
            && known.hint == hint
        {
            known.last_seen = now;
            return None;
        }
        self.sightings.insert(
            key.clone(),
            Sighting {
                beacon,
                hint: hint.clone(),
                last_seen: now,
            },
        );
        Some(PresenceEvent::Seen { key, beacon, hint })
    }

    pub(super) fn forget(&mut self, key: &PresenceKey) -> Option<PresenceEvent> {
        self.sightings
            .remove(key)
            .map(|sighting| PresenceEvent::Lost {
                key: key.clone(),
                beacon: sighting.beacon,
            })
    }

    /// Drops sightings not refreshed within `max_age`, for planes with no removal signal.
    pub(super) fn expire(&mut self, now: Instant, max_age: Duration) -> Vec<PresenceEvent> {
        let stale: Vec<PresenceKey> = self
            .sightings
            .iter()
            .filter(|(_, sighting)| now.saturating_duration_since(sighting.last_seen) > max_age)
            .map(|(key, _)| key.clone())
            .collect();
        stale.iter().filter_map(|key| self.forget(key)).collect()
    }
}

#[cfg(test)]
mod tests {
    use llts_signaling::device::DeviceClass;
    use llts_signaling::discovery::{PairingBeacon, PairingFlags, PairingNonce};

    use super::*;

    fn beacon(nonce: u8) -> Beacon {
        Beacon::Pairing(PairingBeacon {
            device_class: DeviceClass::Phone,
            flags: PairingFlags::NEARBY_CODE,
            nonce: PairingNonce([nonce; 8]),
        })
    }

    fn lan(port: u16) -> AddressHint {
        AddressHint::Lan {
            addresses: vec![IpAddr::from([192, 168, 1, 20])],
            port,
        }
    }

    #[test]
    fn repeated_sightings_are_reported_once() {
        let mut tracker = PresenceTracker::default();
        let key = PresenceKey::Mdns("a._llts._udp.local.".into());
        let now = Instant::now();
        assert!(tracker
            .observe(key.clone(), beacon(1), lan(1), now)
            .is_some());
        assert!(tracker
            .observe(key.clone(), beacon(1), lan(1), now)
            .is_none());
        assert!(tracker
            .observe(key.clone(), beacon(2), lan(1), now)
            .is_some());
        assert!(tracker.observe(key, beacon(2), lan(2), now).is_some());
    }

    #[test]
    fn forgetting_reports_the_last_beacon() {
        let mut tracker = PresenceTracker::default();
        let key = PresenceKey::Mdns("a".into());
        tracker.observe(key.clone(), beacon(3), lan(1), Instant::now());
        assert_eq!(
            tracker.forget(&key),
            Some(PresenceEvent::Lost {
                key: key.clone(),
                beacon: beacon(3)
            })
        );
        assert_eq!(tracker.forget(&key), None);
    }

    #[test]
    fn stale_sightings_expire_and_fresh_ones_stay() {
        let mut tracker = PresenceTracker::default();
        let start = Instant::now();
        let later = start + Duration::from_secs(40);
        tracker.observe(PresenceKey::Mdns("old".into()), beacon(1), lan(1), start);
        tracker.observe(PresenceKey::Mdns("new".into()), beacon(2), lan(1), later);
        let lost = tracker.expire(later, Duration::from_secs(30));
        assert_eq!(
            lost,
            [PresenceEvent::Lost {
                key: PresenceKey::Mdns("old".into()),
                beacon: beacon(1)
            }]
        );
        assert!(tracker.expire(later, Duration::from_secs(30)).is_empty());
    }
}
