/// Rounds toward zero, saturating at the `i32` range (NaN becomes 0), as Rust float casts do.
#[expect(
    clippy::cast_possible_truncation,
    reason = "saturating conversion is the intent"
)]
pub const fn saturating_i32(value: f64) -> i32 {
    value as i32
}

/// Narrows to `f32`, accepting the precision loss.
#[expect(
    clippy::cast_possible_truncation,
    reason = "f32 precision is enough for the callers"
)]
pub const fn narrow_f32(value: f64) -> f32 {
    value as f32
}
