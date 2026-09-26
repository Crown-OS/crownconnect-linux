use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, or zero on a clock set before it.
pub fn unix_millis_now() -> u64 {
    duration_millis(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default(),
    )
}

/// Whole milliseconds, saturating at `u64::MAX`.
pub fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_saturate_instead_of_wrapping() {
        assert_eq!(duration_millis(Duration::from_millis(1_500)), 1_500);
        assert_eq!(duration_millis(Duration::MAX), u64::MAX);
    }
}
