use serde::{Deserialize, Serialize};

/// A slice of a peer's state that this computer caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Topic {
    Battery,
    Media,
    Volume,
    Clipboard,
    Hotspot,
    Calls,
}
