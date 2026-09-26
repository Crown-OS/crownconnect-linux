//! Which features the user allows each paired device, kept across restarts.

use std::collections::BTreeMap;
use std::path::PathBuf;

use llts_signaling::device::{DeviceId, FeatureSet};

use crate::util::private_file::{read_if_present, write_private};

#[derive(Debug, thiserror::Error)]
pub(crate) enum FeatureStoreError {
    #[error("feature choices at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("feature choices at {path} are corrupt: {source}")]
    Corrupt {
        path: PathBuf,
        #[source]
        source: postcard::Error,
    },
}

/// A postcard map from device to its allowed features, rewritten whole on every change.
#[derive(Debug)]
pub(crate) struct FeatureStore {
    path: PathBuf,
    features: BTreeMap<DeviceId, FeatureSet>,
}

impl FeatureStore {
    pub(crate) fn open(path: PathBuf) -> Result<Self, FeatureStoreError> {
        let features = match read_if_present(&path) {
            Ok(Some(bytes)) => {
                postcard::from_bytes(&bytes).map_err(|source| FeatureStoreError::Corrupt {
                    path: path.clone(),
                    source,
                })?
            }
            Ok(None) => BTreeMap::new(),
            Err(source) => return Err(FeatureStoreError::Io { path, source }),
        };
        Ok(Self { path, features })
    }

    /// An empty store at `path`, for when the stored one could not be read.
    pub(crate) const fn empty(path: PathBuf) -> Self {
        Self {
            path,
            features: BTreeMap::new(),
        }
    }

    pub(crate) fn enabled(&self, id: &DeviceId) -> Option<FeatureSet> {
        self.features.get(id).copied()
    }

    pub(crate) fn set(
        &mut self,
        id: DeviceId,
        enabled: FeatureSet,
    ) -> Result<(), FeatureStoreError> {
        if self.features.insert(id, enabled) == Some(enabled) {
            return Ok(());
        }
        self.save()
    }

    pub(crate) fn remove(&mut self, id: &DeviceId) -> Result<(), FeatureStoreError> {
        if self.features.remove(id).is_none() {
            return Ok(());
        }
        self.save()
    }

    fn save(&self) -> Result<(), FeatureStoreError> {
        let io = |source| FeatureStoreError::Io {
            path: self.path.clone(),
            source,
        };
        let bytes = postcard::to_stdvec(&self.features)
            .map_err(|source| io(std::io::Error::other(source)))?;
        write_private(&self.path, &bytes).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use llts_signaling::device::Feature;

    use super::*;

    #[test]
    fn choices_survive_a_reopen_and_forgetting_removes_them() -> Result<(), FeatureStoreError> {
        let path = std::env::temp_dir()
            .join(format!("crownconnect-features-{}", std::process::id()))
            .join("features.pc");
        let phone = DeviceId([4; 32]);
        let choice = FeatureSet::EMPTY.with(Feature::Battery);
        let mut store = FeatureStore::open(path.clone())?;
        assert_eq!(store.enabled(&phone), None);
        store.set(phone, choice)?;
        assert_eq!(
            FeatureStore::open(path.clone())?.enabled(&phone),
            Some(choice)
        );
        store.remove(&phone)?;
        assert_eq!(FeatureStore::open(path.clone())?.enabled(&phone), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(&path));
        Ok(())
    }
}
