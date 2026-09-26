//! This computer's long-term X25519 identity: its device id on every llts and ffsp link.

use std::path::{Path, PathBuf};

use llts_transport::security::{DeviceIdentity, StaticKey};
use llts_transport::TransportError;
use zeroize::Zeroizing;

use crate::pairing::ffsp_bridge::{FfspBridgeError, FfspLinkStore};
use crate::util::private_file::{read_if_present, write_private};

const KEY_LEN: usize = StaticKey::LEN;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("identity file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("identity file {0} is corrupt")]
    Corrupt(PathBuf),
    #[error("cannot generate an identity: {0}")]
    Generate(#[from] TransportError),
    #[error("cannot read ffsp's identity: {0}")]
    Ffsp(#[from] FfspBridgeError),
}

/// The identity stored at `path`. On first run it adopts the one ffsp generated, so both
/// protocols know this computer by the same key, or generates a new one; either is saved
/// readable only by the owner.
///
/// # Errors
///
/// Fails when the file is unreadable or corrupt, or a new identity cannot be saved.
pub fn load_or_create(path: &Path, ffsp: &FfspLinkStore) -> Result<DeviceIdentity, IdentityError> {
    let io = |source| IdentityError::Io {
        path: path.to_owned(),
        source,
    };
    if let Some(bytes) = read_if_present(path).map_err(io)? {
        return decode(&Zeroizing::new(bytes))
            .ok_or_else(|| IdentityError::Corrupt(path.to_owned()));
    }
    let identity = match ffsp.identity()? {
        Some(stored) => {
            DeviceIdentity::from_parts(*stored.secret_key, StaticKey(stored.public_key))
        }
        None => DeviceIdentity::generate()?,
    };
    write_private(path, &encode(&identity)).map_err(io)?;
    Ok(identity)
}

fn encode(identity: &DeviceIdentity) -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(KEY_LEN * 2));
    bytes.extend_from_slice(identity.private());
    bytes.extend_from_slice(identity.public().as_bytes());
    bytes
}

fn decode(bytes: &[u8]) -> Option<DeviceIdentity> {
    let (private, public) = bytes.split_first_chunk::<KEY_LEN>()?;
    let public: [u8; KEY_LEN] = public.try_into().ok()?;
    Some(DeviceIdentity::from_parts(*private, StaticKey(public)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "crownconnect-identity-{}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn an_identity_is_created_once_and_then_reloaded() -> Result<(), IdentityError> {
        let directory = scratch("reload");
        let ffsp = FfspLinkStore::new(directory.join("links"));
        let path = directory.join("identity");
        let first = load_or_create(&path, &ffsp)?;
        let again = load_or_create(&path, &ffsp)?;
        assert_eq!(first.public(), again.public());
        assert_eq!(first.private(), again.private());
        let _ = std::fs::remove_dir_all(&directory);
        Ok(())
    }

    #[test]
    fn a_truncated_identity_is_corrupt() {
        assert!(decode(&[1; KEY_LEN + 3]).is_none());
        assert!(decode(&[1; KEY_LEN * 2]).is_some());
    }
}
