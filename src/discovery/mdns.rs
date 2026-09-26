//! The LAN plane: the same beacons in the TXT record of a `_llts._udp` service, next to the llts
//! UDP port. Instance and host names are random per advertisement, so they identify nothing.

use std::time::Instant;

use llts_signaling::discovery::{Beacon, TxtRecord, SERVICE_TYPE};
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::{mpsc, watch};

use super::presence::{AddressHint, PresenceEvent, PresenceKey, PresenceTracker};
use crate::util::random::random_hex;

const NAME_BYTES: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum MdnsError {
    #[error("mDNS: {0}")]
    Mdns(#[from] mdns_sd::Error),
    #[error("mDNS: {0}")]
    Io(#[from] std::io::Error),
}

fn service_domain() -> String {
    format!("{SERVICE_TYPE}.local.")
}

/// Registers one service per beacon in `advertised`, with `port` in its TXT record, and reports
/// llts services seen on the network, until either channel closes.
///
/// # Errors
///
/// Fails when no mDNS socket can be opened.
pub async fn run(
    mut advertised: watch::Receiver<Vec<Beacon>>,
    port: u16,
    presence: mpsc::Sender<PresenceEvent>,
) -> Result<(), MdnsError> {
    let daemon = ServiceDaemon::new()?;
    let domain = service_domain();
    let browse = daemon.browse(&domain)?;
    let mut registered = register(&daemon, &domain, &advertised.borrow_and_update(), port);
    let mut tracker = PresenceTracker::default();
    let outcome = loop {
        let report = tokio::select! {
            event = browse.recv_async() => match event {
                Ok(event) => observe(&mut tracker, &registered, event),
                Err(_) => break Ok(()),
            },
            changed = advertised.changed() => {
                if changed.is_err() {
                    break Ok(());
                }
                unregister(&daemon, &registered);
                registered = register(&daemon, &domain, &advertised.borrow_and_update(), port);
                None
            }
        };
        if let Some(report) = report
            && presence.send(report).await.is_err()
        {
            break Ok(());
        }
    };
    unregister(&daemon, &registered);
    let _ = daemon.shutdown();
    outcome
}

fn observe(
    tracker: &mut PresenceTracker,
    own: &[String],
    event: ServiceEvent,
) -> Option<PresenceEvent> {
    match event {
        ServiceEvent::ServiceResolved(service) if !own.contains(&service.fullname) => {
            let (beacon, hint) = sighting(&service)?;
            tracker.observe(
                PresenceKey::Mdns(service.fullname),
                beacon,
                hint,
                Instant::now(),
            )
        }
        ServiceEvent::ServiceRemoved(_, fullname) => tracker.forget(&PresenceKey::Mdns(fullname)),
        _ => None,
    }
}

fn sighting(service: &ResolvedService) -> Option<(Beacon, AddressHint)> {
    let record = TxtRecord::parse(|key| service.get_property_val_str(key)).ok()?;
    let addresses = service
        .get_addresses()
        .iter()
        .map(mdns_sd::ScopedIp::to_ip_addr)
        .collect();
    Some((
        record.beacon,
        AddressHint::Lan {
            addresses,
            port: record.port,
        },
    ))
}

fn register(daemon: &ServiceDaemon, domain: &str, beacons: &[Beacon], port: u16) -> Vec<String> {
    beacons
        .iter()
        .filter_map(|beacon| match service_info(domain, *beacon, port) {
            Ok(info) => {
                let fullname = info.get_fullname().to_owned();
                match daemon.register(info) {
                    Ok(()) => Some(fullname),
                    Err(error) => {
                        tracing::warn!(%error, "cannot register an mDNS beacon");
                        None
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "cannot describe an mDNS beacon");
                None
            }
        })
        .collect()
}

fn service_info(domain: &str, beacon: Beacon, port: u16) -> Result<ServiceInfo, MdnsError> {
    let name = random_hex(NAME_BYTES)?;
    let host = format!("{name}.local.");
    let pairs = TxtRecord { beacon, port }.to_pairs();
    Ok(ServiceInfo::new(domain, &name, &host, "", port, &pairs[..])?.enable_addr_auto())
}

fn unregister(daemon: &ServiceDaemon, registered: &[String]) {
    for fullname in registered {
        let _ = daemon.unregister(fullname);
    }
}

#[cfg(test)]
mod tests {
    use llts_signaling::device::DeviceClass;
    use llts_signaling::discovery::{PairingBeacon, PairingFlags, PairingNonce};

    use super::*;

    #[test]
    fn services_are_registered_under_the_llts_type_with_random_names() -> Result<(), MdnsError> {
        let beacon = Beacon::Pairing(PairingBeacon {
            device_class: DeviceClass::Computer,
            flags: PairingFlags::QR,
            nonce: PairingNonce([1; 8]),
        });
        let first = service_info(&service_domain(), beacon, 47_470)?;
        let second = service_info(&service_domain(), beacon, 47_470)?;
        assert!(first.get_fullname().ends_with("._llts._udp.local."));
        assert_ne!(first.get_fullname(), second.get_fullname());
        let parsed = TxtRecord::parse(|key| first.get_property_val_str(key));
        assert_eq!(
            parsed,
            Ok(TxtRecord {
                beacon,
                port: 47_470
            })
        );
        Ok(())
    }
}
