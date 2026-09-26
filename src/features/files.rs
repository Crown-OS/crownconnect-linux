//! Sending a file: llts only announces it with a `FileOffer`; the bytes go over ffsp, through
//! the `fileshare-linux` command line when it is installed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FILESHARE_BINARY: &str = "fileshare-linux";
const FILESHARE_OVERRIDE: &str = "CROWNCONNECT_FILESHARE";
/// Wi-Fi reaches every paired device on the LAN without a Bluetooth or USB setup step.
const FILESHARE_MEDIUMS: &str = "wifi";

/// What happened to the ffsp side of a file offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfspSend {
    /// `fileshare-linux send` runs in the background, waiting for the peer to connect.
    Started,
    /// No ffsp sender is installed; the peer only sees the offer.
    Unavailable,
}

/// `CROWNCONNECT_FILESHARE`, else `fileshare-linux` on `PATH`.
fn fileshare_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(FILESHARE_OVERRIDE) {
        return Some(PathBuf::from(path));
    }
    let search: OsString = std::env::var_os("PATH")?;
    std::env::split_paths(&search)
        .map(|directory| directory.join(FILESHARE_BINARY))
        .find(|candidate| candidate.is_file())
}

/// Starts sending `path` over ffsp. The command line accepts whichever paired device connects
/// first and names its own transfer id, so the offer's id does not select anything yet.
///
/// # Errors
///
/// Fails when the sender is installed but cannot be started.
pub fn start_ffsp_send(path: &Path) -> std::io::Result<FfspSend> {
    let Some(binary) = fileshare_binary() else {
        return Ok(FfspSend::Unavailable);
    };
    let mut child = Command::new(binary)
        .arg("--mediums")
        .arg(FILESHARE_MEDIUMS)
        .arg("send")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::Builder::new()
        .name("crownconnect-ffsp-send".to_owned())
        .spawn(move || match child.wait() {
            Ok(status) if status.success() => tracing::info!("ffsp transfer finished"),
            Ok(status) => tracing::warn!(%status, "ffsp transfer failed"),
            Err(error) => tracing::warn!(%error, "ffsp transfer vanished"),
        })?;
    Ok(FfspSend::Started)
}
