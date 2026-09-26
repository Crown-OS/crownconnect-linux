use std::collections::HashMap;

use llts_signaling::message::Media;
use zbus::names::OwnedBusName;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{ObjectPath, OwnedValue};

use super::metadata::TrackMetadata;
use super::selection::{seek_target_us, PlaybackState};
use crate::state::MediaSnapshot;

#[zbus::proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait Player {
    fn play(&self) -> zbus::Result<()>;
    fn pause(&self) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn previous(&self) -> zbus::Result<()>;
    fn set_position(&self, track_id: &ObjectPath<'_>, position: i64) -> zbus::Result<()>;
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
    #[zbus(property)]
    fn position(&self) -> zbus::Result<i64>;
}

#[zbus::proxy(
    interface = "org.mpris.MediaPlayer2",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait Application {
    #[zbus(property)]
    fn identity(&self) -> zbus::Result<String>;
}

/// One MPRIS player on the session bus.
#[derive(Debug)]
pub(super) struct MprisPlayer {
    pub(super) name: OwnedBusName,
    player: PlayerProxy<'static>,
    identity: String,
}

impl MprisPlayer {
    pub(super) async fn connect(
        connection: &zbus::Connection,
        name: OwnedBusName,
    ) -> zbus::Result<Self> {
        let player = PlayerProxy::builder(connection)
            .destination(name.clone())?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let application = ApplicationProxy::builder(connection)
            .destination(name.clone())?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let identity = match application.identity().await {
            Ok(identity) => identity,
            Err(_) => fallback_identity(name.as_str()).to_owned(),
        };
        Ok(Self {
            name,
            player,
            identity,
        })
    }

    pub(super) async fn state(&self) -> PlaybackState {
        self.player
            .playback_status()
            .await
            .map_or(PlaybackState::Stopped, |status| {
                PlaybackState::parse(&status)
            })
    }

    pub(super) async fn snapshot(&self, state: PlaybackState) -> zbus::Result<MediaSnapshot> {
        let track = TrackMetadata::from_map(&self.player.metadata().await?);
        let position_us = self.player.position().await.unwrap_or_default();
        Ok(MediaSnapshot {
            app: self.identity.clone(),
            title: track.title,
            artist: track.artist,
            position_ms: u64::try_from(position_us / 1_000).unwrap_or_default(),
            duration_ms: track.length_us.unwrap_or_default() / 1_000,
            playing: state == PlaybackState::Playing,
        })
    }

    pub(super) async fn execute(&self, command: Media) -> zbus::Result<()> {
        match command {
            Media::Play => self.player.play().await,
            Media::Pause => self.player.pause().await,
            Media::Next => self.player.next().await,
            Media::Previous => self.player.previous().await,
            Media::Seek { percent } => {
                let track = TrackMetadata::from_map(&self.player.metadata().await?);
                match (track.track_id, track.length_us) {
                    (Some(track_id), Some(length_us)) => {
                        self.player
                            .set_position(&track_id, seek_target_us(length_us, percent))
                            .await
                    }
                    _ => Ok(()),
                }
            }
        }
    }
}

fn fallback_identity(bus_name: &str) -> &str {
    bus_name
        .strip_prefix(super::MPRIS_PREFIX)
        .and_then(|rest| rest.split('.').next())
        .unwrap_or(bus_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bus_name_names_a_player_without_an_identity() {
        assert_eq!(
            fallback_identity("org.mpris.MediaPlayer2.chromium.instance3155"),
            "chromium"
        );
        assert_eq!(fallback_identity("odd"), "odd");
    }
}
