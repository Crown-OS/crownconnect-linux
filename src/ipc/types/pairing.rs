use serde::{Deserialize, Serialize};

/// A one-time invitation for a new device to pair by scanning a QR code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingOffer {
    /// The text to render as a QR code; it carries a one-time secret, so it must not be logged.
    pub qr: String,
    pub expires_unix_ms: u64,
}
