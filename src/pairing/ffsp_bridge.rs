//! Makes a device paired over llts trusted by ffsp too, so file transfers need no second pairing.
//!
//! ffsp stays a separate protocol with its own link secret, derived here from the llts one and
//! written straight into ffsp's link store (`ffsp-linux`'s `FileLinkStore` format). Secrets
//! never cross IPC.

use std::path::PathBuf;

use llts_signaling::device::DeviceClass;
use zeroize::Zeroizing;

use crate::util::private_file::{read_if_present, write_private};

/// ffsp's context for link secrets; deriving under it keeps the ffsp secret independent of the
/// llts one while both ends compute the same value.
pub const FFSP_LINK_SECRET_CONTEXT: &str = "crownos.ffsp.v1 link secret";
const FFSP_DEVICE_ID_CONTEXT: &str = "crownos.ffsp.v1.device-id";

const KEY_LEN: usize = 32;
const HEADER_LEN: usize = KEY_LEN * 3 + 2 + 1 + 2;
const IDENTITY_LEN: usize = KEY_LEN * 2;

/// ffsp mediums a paired device is assumed to support: Wi-Fi infrastructure and BLE.
const DEFAULT_CAPABILITIES: u16 = (1 << 0) | (1 << 2);

const FFSP_LAPTOP: u8 = 1;
const FFSP_PHONE: u8 = 3;
const FFSP_TABLET: u8 = 4;

#[derive(Debug, thiserror::Error)]
pub enum FfspBridgeError {
    #[error("ffsp link store: {0}")]
    Io(#[from] std::io::Error),
    #[error("ffsp link store is corrupt at record {0}")]
    Corrupt(usize),
}

/// The ffsp link secret for one pairing, wiped on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct FfspLinkSecret(Zeroizing<[u8; KEY_LEN]>);

impl std::fmt::Debug for FfspLinkSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FfspLinkSecret(..)")
    }
}

impl FfspLinkSecret {
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

pub fn derive_ffsp_link_secret(llts_link_secret: &[u8; KEY_LEN]) -> FfspLinkSecret {
    FfspLinkSecret(Zeroizing::new(blake3::derive_key(
        FFSP_LINK_SECRET_CONTEXT,
        llts_link_secret,
    )))
}

/// ffsp names devices by a hash of their public key rather than the key itself.
pub fn ffsp_device_id(public_key: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    blake3::derive_key(FFSP_DEVICE_ID_CONTEXT, public_key)
}

const fn ffsp_class(class: DeviceClass) -> u8 {
    match class {
        DeviceClass::Computer => FFSP_LAPTOP,
        DeviceClass::Tablet => FFSP_TABLET,
        DeviceClass::Phone | DeviceClass::Watch => FFSP_PHONE,
    }
}

/// A paired device as ffsp should remember it.
#[derive(Debug)]
pub struct FfspLink<'a> {
    pub public_key: [u8; KEY_LEN],
    pub name: &'a str,
    pub class: DeviceClass,
    pub secret: FfspLinkSecret,
}

/// This device's X25519 identity as ffsp stores it, wiped on drop.
#[derive(Debug)]
pub struct StoredIdentity {
    pub secret_key: Zeroizing<[u8; KEY_LEN]>,
    pub public_key: [u8; KEY_LEN],
}

struct StoredLink {
    device: [u8; KEY_LEN],
    public_key: [u8; KEY_LEN],
    secret: Zeroizing<[u8; KEY_LEN]>,
    capabilities: u16,
    class: u8,
    name: String,
}

/// ffsp's link store on disk: `$XDG_DATA_HOME/crownos/fileshare/links` by default.
#[derive(Debug, Clone)]
pub struct FfspLinkStore {
    path: PathBuf,
}

impl FfspLinkStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Adds or replaces `link`, rewriting the store atomically with owner-only permissions.
    ///
    /// # Errors
    ///
    /// Fails when the store cannot be read, is corrupt, or cannot be written.
    pub fn record(&self, link: &FfspLink<'_>) -> Result<(), FfspBridgeError> {
        let device = ffsp_device_id(&link.public_key);
        let mut links = self.load()?;
        links.retain(|stored| stored.device != device);
        links.push(StoredLink {
            device,
            public_key: link.public_key,
            secret: Zeroizing::new(*link.secret.as_bytes()),
            capabilities: DEFAULT_CAPABILITIES,
            class: ffsp_class(link.class),
            name: link.name.to_owned(),
        });
        Ok(write_private(&self.path, &encode(&links))?)
    }

    /// Removes the device with `public_key`; `false` when ffsp did not know it.
    ///
    /// # Errors
    ///
    /// Fails when the store cannot be read, is corrupt, or cannot be written.
    pub fn forget(&self, public_key: &[u8; KEY_LEN]) -> Result<bool, FfspBridgeError> {
        let device = ffsp_device_id(public_key);
        let mut links = self.load()?;
        let before = links.len();
        links.retain(|stored| stored.device != device);
        if links.len() == before {
            return Ok(false);
        }
        write_private(&self.path, &encode(&links))?;
        Ok(true)
    }

    /// The identity ffsp generated for this device, if it has run before.
    ///
    /// # Errors
    ///
    /// Fails when the identity file exists but cannot be read.
    pub fn identity(&self) -> Result<Option<StoredIdentity>, FfspBridgeError> {
        let Some(bytes) = read_if_present(&self.path.with_extension("identity"))? else {
            return Ok(None);
        };
        let bytes = Zeroizing::new(bytes);
        let Some((secret, public)) = bytes
            .get(..IDENTITY_LEN)
            .filter(|_| bytes.len() == IDENTITY_LEN)
            .map(|pair| pair.split_at(KEY_LEN))
        else {
            return Ok(None);
        };
        let mut secret_key = Zeroizing::new([0; KEY_LEN]);
        secret_key.copy_from_slice(secret);
        let mut public_key = [0; KEY_LEN];
        public_key.copy_from_slice(public);
        Ok(Some(StoredIdentity {
            secret_key,
            public_key,
        }))
    }

    fn load(&self) -> Result<Vec<StoredLink>, FfspBridgeError> {
        read_if_present(&self.path)?
            .map_or_else(|| Ok(Vec::new()), |bytes| decode(&Zeroizing::new(bytes)))
    }
}

fn encode(links: &[StoredLink]) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(links.len() * (HEADER_LEN + KEY_LEN)));
    for link in links {
        let name = link.name.as_bytes();
        let name = name.get(..usize::from(u16::MAX)).unwrap_or(name);
        out.extend_from_slice(&link.device);
        out.extend_from_slice(&link.public_key);
        out.extend_from_slice(&*link.secret);
        out.extend_from_slice(&link.capabilities.to_le_bytes());
        out.push(link.class);
        out.extend_from_slice(&u16::try_from(name.len()).unwrap_or(u16::MAX).to_le_bytes());
        out.extend_from_slice(name);
    }
    out
}

fn decode(mut bytes: &[u8]) -> Result<Vec<StoredLink>, FfspBridgeError> {
    let mut links = Vec::new();
    while !bytes.is_empty() {
        let corrupt = FfspBridgeError::Corrupt(links.len());
        let (header, rest) = bytes.split_at_checked(HEADER_LEN).ok_or(corrupt)?;
        let key = |at: usize| -> [u8; KEY_LEN] {
            let mut key = [0; KEY_LEN];
            if let Some(source) = header.get(at..at + KEY_LEN) {
                key.copy_from_slice(source);
            }
            key
        };
        let &[.., cap_lo, cap_hi, class, len_lo, len_hi] = header else {
            return Err(FfspBridgeError::Corrupt(links.len()));
        };
        let name_len = usize::from(u16::from_le_bytes([len_lo, len_hi]));
        let (name, rest) = rest
            .split_at_checked(name_len)
            .ok_or(FfspBridgeError::Corrupt(links.len()))?;
        links.push(StoredLink {
            device: key(0),
            public_key: key(KEY_LEN),
            secret: Zeroizing::new(key(KEY_LEN * 2)),
            capabilities: u16::from_le_bytes([cap_lo, cap_hi]),
            class,
            name: String::from_utf8(name.to_vec())
                .map_err(|_| FfspBridgeError::Corrupt(links.len()))?,
        });
        bytes = rest;
    }
    Ok(links)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("crownconnect-ffsp-{}-{name}", std::process::id()))
            .join("links")
    }

    fn link(public_key: u8, name: &str) -> FfspLink<'_> {
        FfspLink {
            public_key: [public_key; KEY_LEN],
            name,
            class: DeviceClass::Phone,
            secret: derive_ffsp_link_secret(&[public_key; KEY_LEN]),
        }
    }

    #[test]
    fn the_ffsp_secret_is_a_keyed_derivation_of_the_llts_one() {
        let llts = [7; KEY_LEN];
        let derived = derive_ffsp_link_secret(&llts);
        assert_eq!(derived, derive_ffsp_link_secret(&llts));
        assert_ne!(derived.as_bytes(), &llts);
        assert_eq!(
            derived.as_bytes(),
            &blake3::derive_key(FFSP_LINK_SECRET_CONTEXT, &llts)
        );
    }

    #[test]
    fn records_are_upserted_and_forgotten() -> Result<(), FfspBridgeError> {
        let path = scratch("upsert");
        let store = FfspLinkStore::new(&path);
        store.record(&link(1, "Pixel"))?;
        store.record(&link(2, "Tab"))?;
        store.record(&link(1, "Pixel 9"))?;
        let names: Vec<String> = store
            .load()?
            .into_iter()
            .map(|stored| stored.name)
            .collect();
        assert_eq!(names, ["Tab", "Pixel 9"]);
        assert!(store.forget(&[2; KEY_LEN])?);
        assert!(!store.forget(&[2; KEY_LEN])?);
        assert_eq!(store.load()?.len(), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(&path));
        Ok(())
    }

    #[test]
    fn the_store_is_private_to_its_owner() -> Result<(), FfspBridgeError> {
        use std::os::unix::fs::PermissionsExt;
        let path = scratch("mode");
        FfspLinkStore::new(&path).record(&link(3, "Phone"))?;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(&path));
        Ok(())
    }

    #[test]
    fn a_truncated_store_is_reported_corrupt() {
        assert!(matches!(decode(&[0; 10]), Err(FfspBridgeError::Corrupt(0))));
    }
}
