//! Where the daemon keeps its files and what a newly paired device may do.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;

use crate::ipc::proto::{Feature, FeatureSet};
use crate::util::xdg;

/// The UDP port llts listens on unless `CROWNCONNECT_LLTS_PORT` overrides it.
pub const DEFAULT_LLTS_PORT: u16 = 47_470;

const APP_DIRECTORY: &str = "crownos/crownconnect";
const FFSP_LINKS: &str = "crownos/fileshare/links";
const FALLBACK_DEVICE_NAME: &str = "CrownOS computer";

/// Listens on every IPv6 and IPv4 address, so the socket survives address changes.
pub const DEFAULT_LLTS_BIND: IpAddr = IpAddr::V6(Ipv6Addr::UNSPECIFIED);

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("neither XDG_DATA_HOME nor HOME is an absolute path")]
    NoDataDirectory,
    #[error("CROWNCONNECT_LLTS_PORT is not a port number: {0}")]
    InvalidPort(String),
    #[error("CROWNCONNECT_LLTS_BIND is not an IP address: {0}")]
    InvalidBind(String),
    #[error("CROWNCONNECT_STATIC_PEERS holds something other than socket addresses: {0}")]
    InvalidStaticPeer(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPaths {
    /// Private data: the device identity, the llts trust store and per-device feature choices.
    pub data_directory: PathBuf,
    pub identity: PathBuf,
    pub trust_store: PathBuf,
    pub peer_features: PathBuf,
    /// ffsp's link store, which pairing writes into so file transfers trust the same devices.
    pub ffsp_links: PathBuf,
    /// Overrides the crownos-ipc socket directory, for running several daemons side by side.
    pub ipc_directory: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    pub paths: DaemonPaths,
    pub llts_port: u16,
    /// The address the llts socket binds, with `llts_port`.
    pub llts_bind: IpAddr,
    /// Where to look for paired devices besides BLE and mDNS, for networks without multicast
    /// and for tests.
    pub static_peers: Vec<SocketAddr>,
    pub device_name: String,
    /// What a device may use right after pairing, until the user changes it.
    pub default_features: FeatureSet,
    /// The v4l2loopback node a peer's camera appears on (`CROWNCONNECT_CAMERA_DEVICE`); the
    /// first loopback node there is when unset.
    pub camera_device: Option<PathBuf>,
}

impl DaemonConfig {
    /// # Errors
    ///
    /// Fails when there is no usable data directory or the port override is malformed.
    pub fn from_environment() -> Result<Self, ConfigError> {
        let data_home = xdg::base_directory("XDG_DATA_HOME", ".local/share")
            .ok_or(ConfigError::NoDataDirectory)?;
        let data_directory = data_home.join(APP_DIRECTORY);
        Ok(Self {
            paths: DaemonPaths::in_directories(
                data_directory,
                &data_home,
                std::env::var_os("CROWNCONNECT_IPC_DIR").map(PathBuf::from),
            ),
            llts_port: llts_port(std::env::var("CROWNCONNECT_LLTS_PORT").ok().as_deref())?,
            llts_bind: llts_bind(std::env::var("CROWNCONNECT_LLTS_BIND").ok().as_deref())?,
            static_peers: static_peers(std::env::var("CROWNCONNECT_STATIC_PEERS").ok().as_deref())?,
            device_name: device_name(),
            default_features: default_features(),
            camera_device: std::env::var_os("CROWNCONNECT_CAMERA_DEVICE").map(PathBuf::from),
        })
    }
}

impl DaemonPaths {
    /// The daemon's files under `data_directory`, next to ffsp's under `data_home`.
    pub fn in_directories(
        data_directory: PathBuf,
        data_home: &std::path::Path,
        ipc_directory: Option<PathBuf>,
    ) -> Self {
        Self {
            identity: data_directory.join("identity"),
            trust_store: data_directory.join("trust.pc"),
            peer_features: data_directory.join("features.pc"),
            ffsp_links: data_home.join(FFSP_LINKS),
            ipc_directory,
            data_directory,
        }
    }
}

impl DaemonConfig {
    pub const fn llts_address(&self) -> SocketAddr {
        SocketAddr::new(self.llts_bind, self.llts_port)
    }
}

fn llts_bind(configured: Option<&str>) -> Result<IpAddr, ConfigError> {
    configured.map_or(Ok(DEFAULT_LLTS_BIND), |value| {
        value
            .trim()
            .parse()
            .map_err(|_| ConfigError::InvalidBind(value.to_owned()))
    })
}

fn static_peers(configured: Option<&str>) -> Result<Vec<SocketAddr>, ConfigError> {
    configured
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            entry
                .parse()
                .map_err(|_| ConfigError::InvalidStaticPeer(entry.to_owned()))
        })
        .collect()
}

fn llts_port(configured: Option<&str>) -> Result<u16, ConfigError> {
    configured.map_or(Ok(DEFAULT_LLTS_PORT), |value| {
        value
            .trim()
            .parse()
            .map_err(|_| ConfigError::InvalidPort(value.to_owned()))
    })
}

fn device_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| FALLBACK_DEVICE_NAME.to_owned())
}

/// Everything: streaming features still need an explicit start from the user, so allowing
/// them only means the device may be asked.
pub fn default_features() -> FeatureSet {
    Feature::ALL.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_defaults_and_rejects_garbage() {
        assert!(matches!(llts_port(None), Ok(DEFAULT_LLTS_PORT)));
        assert!(matches!(llts_port(Some(" 5000 ")), Ok(5000)));
        assert!(matches!(
            llts_port(Some("70000")),
            Err(ConfigError::InvalidPort(_))
        ));
    }

    #[test]
    fn static_peers_are_a_comma_separated_list() {
        assert!(matches!(static_peers(None), Ok(peers) if peers.is_empty()));
        let peers = static_peers(Some("127.0.0.1:5000, [::1]:6000,")).unwrap_or_default();
        assert_eq!(
            peers,
            [
                SocketAddr::from(([127, 0, 0, 1], 5000)),
                SocketAddr::from((Ipv6Addr::LOCALHOST, 6000))
            ]
        );
        assert!(matches!(
            static_peers(Some("nowhere")),
            Err(ConfigError::InvalidStaticPeer(_))
        ));
        assert!(matches!(llts_bind(None), Ok(DEFAULT_LLTS_BIND)));
        assert!(matches!(
            llts_bind(Some("x")),
            Err(ConfigError::InvalidBind(_))
        ));
    }

    #[test]
    fn new_devices_may_use_every_feature() {
        assert_eq!(default_features().iter().count(), Feature::ALL.len());
    }
}
