//! This computer's Wi-Fi hotspot, from NetworkManager, and turning it on or off for a peer.

mod network_manager;
mod settings;

use futures_util::StreamExt;
use llts_signaling::message::SetHotspot;
use tokio::sync::mpsc;
use zbus::zvariant::{ObjectPath, OwnedObjectPath};

use crate::state::{HotspotSnapshot, LocalState, StateSink};
use network_manager::{
    ActiveConnectionProxy, DeviceProxy, NetworkManagerProxy, SavedConnectionProxy, SettingsProxy,
};
use settings::access_point_ssid;

const WIRELESS_TYPE: &str = "802-11-wireless";
const WIFI_DEVICE_TYPE: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum HotspotError {
    #[error("NetworkManager: {0}")]
    Bus(#[from] zbus::Error),
    #[error("no saved Wi-Fi hotspot connection; create one in network settings first")]
    NoHotspotProfile,
    #[error("this computer has no Wi-Fi device")]
    NoWifiDevice,
}

/// Publishes whether the hotspot is on and applies peers' requests to toggle it.
///
/// # Errors
///
/// Fails without a system bus or NetworkManager.
pub async fn run(
    sink: StateSink,
    mut commands: mpsc::Receiver<SetHotspot>,
) -> Result<(), HotspotError> {
    let connection = zbus::Connection::system().await?;
    let hotspot = Hotspot::new(&connection).await?;
    let mut changes = hotspot.manager.receive_active_connections_changed().await;
    loop {
        let snapshot = hotspot.active().await?.map_or(
            HotspotSnapshot {
                enabled: false,
                ssid: None,
            },
            |(_, ssid)| HotspotSnapshot {
                enabled: true,
                ssid: Some(ssid),
            },
        );
        if !sink.publish(LocalState::Hotspot(snapshot)).await {
            return Ok(());
        }
        tokio::select! {
            Some(_) = changes.next() => {}
            command = commands.recv() => match command {
                Some(SetHotspot { enabled }) => {
                    if let Err(error) = hotspot.set(enabled).await {
                        tracing::warn!(%error, enabled, "cannot toggle the hotspot");
                    }
                }
                None => return Ok(()),
            },
            else => return Ok(()),
        }
    }
}

struct Hotspot<'c> {
    connection: &'c zbus::Connection,
    manager: NetworkManagerProxy<'static>,
}

impl<'c> Hotspot<'c> {
    async fn new(connection: &'c zbus::Connection) -> zbus::Result<Self> {
        Ok(Self {
            connection,
            manager: NetworkManagerProxy::new(connection).await?,
        })
    }

    /// The active access-point connection and its SSID.
    async fn active(&self) -> zbus::Result<Option<(OwnedObjectPath, String)>> {
        for path in self.manager.active_connections().await? {
            let active = ActiveConnectionProxy::builder(self.connection)
                .path(path.clone())?
                .build()
                .await?;
            if active.connection_type().await? != WIRELESS_TYPE {
                continue;
            }
            let saved = active.connection().await?;
            if let Some(ssid) = self.access_point(&saved).await? {
                return Ok(Some((path, ssid)));
            }
        }
        Ok(None)
    }

    async fn access_point(&self, saved: &ObjectPath<'_>) -> zbus::Result<Option<String>> {
        let saved = SavedConnectionProxy::builder(self.connection)
            .path(saved)?
            .build()
            .await?;
        Ok(access_point_ssid(&saved.get_settings().await?))
    }

    async fn set(&self, enabled: bool) -> Result<(), HotspotError> {
        match (enabled, self.active().await?) {
            (true, None) => {
                let profile = self.saved_profile().await?;
                let device = self.wifi_device().await?;
                let root = ObjectPath::from_static_str_unchecked("/");
                self.manager
                    .activate_connection(&profile, &device, &root)
                    .await?;
            }
            (false, Some((active, _))) => self.manager.deactivate_connection(&active).await?,
            _ => {}
        }
        Ok(())
    }

    async fn saved_profile(&self) -> Result<OwnedObjectPath, HotspotError> {
        let settings = SettingsProxy::new(self.connection).await?;
        for path in settings.list_connections().await? {
            if self.access_point(&path).await?.is_some() {
                return Ok(path);
            }
        }
        Err(HotspotError::NoHotspotProfile)
    }

    async fn wifi_device(&self) -> Result<OwnedObjectPath, HotspotError> {
        for path in self.manager.get_devices().await? {
            let device = DeviceProxy::builder(self.connection)
                .path(path.clone())?
                .build()
                .await?;
            if device.device_type().await? == WIFI_DEVICE_TYPE {
                return Ok(path);
            }
        }
        Err(HotspotError::NoWifiDevice)
    }
}
