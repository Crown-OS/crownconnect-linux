use std::fmt;

use serde::{Deserialize, Serialize};

use super::FeatureSet;

/// A device's long-term X25519 public key, which is also its identity on every CrownOS link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceId(pub [u8; 32]);

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

/// What kind of hardware a device is, as it announced itself during pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeviceClass {
    Phone,
    Tablet,
    Watch,
    Computer,
}

/// The path a device was last reached over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LinkKind {
    /// Wi-Fi or Ethernet on the same network.
    Lan,
    Usb,
    /// Presence and control only; media needs a faster link.
    Bluetooth,
}

/// A device's battery as it last reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// Everything a client needs to list a known device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub class: DeviceClass,
    /// Paired and remembered, as opposed to merely discovered nearby.
    pub trusted: bool,
    pub connected: bool,
    pub link: LinkKind,
    pub battery: Option<Battery>,
    /// Features the user allows for this device.
    pub features: FeatureSet,
    /// Features currently running, always a subset of `features`.
    pub active: FeatureSet,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_displays_as_zero_padded_lowercase_hex() {
        assert_eq!(DeviceId([0xab; 32]).to_string(), "ab".repeat(32));
        assert_eq!(DeviceId([0x01; 32]).to_string(), "01".repeat(32));
    }
}
