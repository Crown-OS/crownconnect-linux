use serde::{Deserialize, Serialize};

/// Where a call on a peer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CallStatus {
    Ringing,
    Dialing,
    Active,
    Held,
    Ended,
}

/// One call on a peer's phone line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInfo {
    pub call_id: String,
    pub number: String,
    pub contact_name: Option<String>,
    pub status: CallStatus,
    /// When the call was answered, or `None` while it has not been.
    pub answered_unix_ms: Option<u64>,
}
