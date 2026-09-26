/// Where an MPRIS player's playback stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackState {
    pub(super) fn parse(status: &str) -> Self {
        match status {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }
}

/// Picks the player a paired device should show and control: one that is playing, keeping the
/// current choice while it qualifies, and falling back to a paused player before giving up.
pub(super) fn choose_active<'p>(
    players: &[(&'p str, PlaybackState)],
    current: Option<&str>,
) -> Option<&'p str> {
    let best_with = |state: PlaybackState| {
        players
            .iter()
            .find(|(name, player_state)| *player_state == state && Some(*name) == current)
            .or_else(|| {
                players
                    .iter()
                    .find(|(_, player_state)| *player_state == state)
            })
            .map(|(name, _)| *name)
    };
    best_with(PlaybackState::Playing).or_else(|| best_with(PlaybackState::Paused))
}

/// The `SetPosition` target for seeking to `percent` of a track, clamped to the track.
pub(super) fn seek_target_us(length_us: u64, percent: u8) -> i64 {
    let target = u128::from(length_us) * u128::from(percent.min(100)) / 100;
    i64::try_from(target).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::PlaybackState::{Paused, Playing, Stopped};
    use super::*;

    #[test]
    fn a_playing_player_beats_a_paused_one() {
        let players = [("a", Paused), ("b", Playing)];
        assert_eq!(choose_active(&players, Some("a")), Some("b"));
    }

    #[test]
    fn the_current_choice_is_kept_while_it_plays() {
        let players = [("a", Playing), ("b", Playing)];
        assert_eq!(choose_active(&players, Some("b")), Some("b"));
        assert_eq!(choose_active(&players, None), Some("a"));
    }

    #[test]
    fn paused_players_are_a_fallback_and_stopped_ones_are_not() {
        assert_eq!(
            choose_active(&[("a", Stopped), ("b", Paused)], None),
            Some("b")
        );
        assert_eq!(choose_active(&[("a", Stopped)], Some("a")), None);
        assert_eq!(choose_active(&[], None), None);
    }

    #[test]
    fn seeking_scales_the_track_length_and_clamps_the_percentage() {
        assert_eq!(seek_target_us(200_000_000, 25), 50_000_000);
        assert_eq!(seek_target_us(200_000_000, 250), 200_000_000);
        assert_eq!(seek_target_us(u64::MAX, 100), i64::MAX);
    }

    #[test]
    fn unknown_statuses_count_as_stopped() {
        assert_eq!(PlaybackState::parse("Playing"), Playing);
        assert_eq!(PlaybackState::parse("Paused"), Paused);
        assert_eq!(PlaybackState::parse("whatever"), Stopped);
    }
}
