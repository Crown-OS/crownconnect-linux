/// Rounds to the nearest whole percent within `0..=100`; NaN becomes 0.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped into u8 range first"
)]
pub const fn whole_percent(value: f64) -> u8 {
    if value.is_nan() {
        return 0;
    }
    value.clamp(0.0, 100.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_and_clamps() {
        assert_eq!(whole_percent(41.5), 42);
        assert_eq!(whole_percent(-1.0), 0);
        assert_eq!(whole_percent(250.0), 100);
        assert_eq!(whole_percent(f64::NAN), 0);
    }
}
