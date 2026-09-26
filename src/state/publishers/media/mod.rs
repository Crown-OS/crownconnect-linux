//! What this computer is playing, from MPRIS, and the remote control a paired device drives.

mod metadata;
mod player;
mod selection;

use futures_util::stream::select;
use futures_util::StreamExt;
use llts_signaling::message::Media;
use tokio::sync::mpsc;
use zbus::fdo::{DBusProxy, NameOwnerChanged};
use zbus::names::OwnedBusName;
use zbus::{MatchRule, MessageStream};

use crate::state::{LocalState, MediaSnapshot, StateSink};
use player::MprisPlayer;
use selection::choose_active;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
/// playerctld mirrors whichever player is active; following it too would count every track twice.
const PLAYERCTLD: &str = "org.mpris.MediaPlayer2.playerctld";
const SIGNAL_QUEUE_DEPTH: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum MprisError {
    #[error("session bus: {0}")]
    Bus(#[from] zbus::Error),
    #[error("session bus: {0}")]
    Fdo(#[from] zbus::fdo::Error),
}

/// Publishes the active player on every change and runs the commands paired devices send.
///
/// # Errors
///
/// Fails when the session bus is unreachable.
pub async fn run(sink: StateSink, mut commands: mpsc::Receiver<Media>) -> Result<(), MprisError> {
    let connection = zbus::Connection::session().await?;
    let bus = DBusProxy::new(&connection).await?;
    let mut owner_changes = bus.receive_name_owner_changed().await?;
    let mut signals = select(
        watch(
            &connection,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
        )
        .await?,
        watch(&connection, PLAYER_INTERFACE, "Seeked").await?,
    );
    let mut players = Players::new(connection);
    for name in bus.list_names().await? {
        players.add(name).await;
    }
    loop {
        if !sink
            .publish(LocalState::Media(players.active_snapshot().await))
            .await
        {
            return Ok(());
        }
        tokio::select! {
            Some(change) = owner_changes.next() => players.apply(&change).await,
            Some(_) = signals.next() => {}
            command = commands.recv() => match command {
                Some(command) => players.execute(command).await,
                None => return Ok(()),
            },
            else => return Ok(()),
        }
    }
}

async fn watch(
    connection: &zbus::Connection,
    interface: &'static str,
    member: &'static str,
) -> zbus::Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(interface)?
        .member(member)?
        .path(MPRIS_PATH)?
        .build();
    MessageStream::for_match_rule(rule, connection, Some(SIGNAL_QUEUE_DEPTH)).await
}

fn is_player(name: &str) -> bool {
    name.starts_with(MPRIS_PREFIX) && name != PLAYERCTLD
}

#[derive(Debug)]
struct Players {
    connection: zbus::Connection,
    players: Vec<MprisPlayer>,
    active: Option<OwnedBusName>,
}

impl Players {
    const fn new(connection: zbus::Connection) -> Self {
        Self {
            connection,
            players: Vec::new(),
            active: None,
        }
    }

    async fn add(&mut self, name: OwnedBusName) {
        if !is_player(name.as_str()) {
            return;
        }
        self.remove(name.as_str());
        match MprisPlayer::connect(&self.connection, name).await {
            Ok(player) => self.players.push(player),
            Err(error) => tracing::debug!(%error, "skipping an MPRIS player"),
        }
    }

    fn remove(&mut self, name: &str) {
        self.players.retain(|player| player.name.as_str() != name);
    }

    async fn apply(&mut self, change: &NameOwnerChanged) {
        let Ok(args) = change.args() else {
            return;
        };
        let name = args.name().as_str();
        if !is_player(name) {
            return;
        }
        match args.new_owner().as_ref() {
            Some(_) => self.add(OwnedBusName::from(args.name().to_owned())).await,
            None => self.remove(name),
        }
    }

    async fn active_snapshot(&mut self) -> Option<MediaSnapshot> {
        let mut states = Vec::with_capacity(self.players.len());
        for player in &self.players {
            states.push((player.name.as_str(), player.state().await));
        }
        let active = choose_active(&states, self.active.as_ref().map(|name| name.as_str()))?;
        let (index, state) = states
            .iter()
            .enumerate()
            .find_map(|(index, (name, state))| (*name == active).then_some((index, *state)))?;
        let player = self.players.get(index)?;
        self.active = Some(player.name.clone());
        player.snapshot(state).await.ok()
    }

    async fn execute(&self, command: Media) {
        let Some(player) = self
            .active
            .as_ref()
            .and_then(|active| self.players.iter().find(|player| player.name == *active))
        else {
            return;
        };
        if let Err(error) = player.execute(command).await {
            tracing::warn!(%error, ?command, "media command failed");
        }
    }
}
