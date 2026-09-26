use crate::util::numeric::saturating_i32;

/// How an encoder follows a new target bitrate from congestion control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateAdjustment {
    Keep,
    /// Shift every frame's QP by this delta without touching rate control state.
    QpOffset(i8),
    /// Rebuild rate control, which costs a keyframe.
    Reconfigure,
}

/// The largest QP shift applied per frame; beyond it the encoder is reconfigured. Six QP steps
/// roughly halve or double the bitrate.
pub const MAX_QP_OFFSET: i8 = 6;

const QP_STEPS_PER_DOUBLING: f64 = 6.0;

pub fn plan_rate_change(
    configured_bps: u32,
    target_bps: u32,
    qp_offsets_supported: bool,
) -> RateAdjustment {
    if configured_bps == 0 || target_bps == 0 {
        return RateAdjustment::Keep;
    }
    let steps = QP_STEPS_PER_DOUBLING * (f64::from(configured_bps) / f64::from(target_bps)).log2();
    let offset = steps.round();
    if offset.abs() < 1.0 {
        RateAdjustment::Keep
    } else if qp_offsets_supported && offset.abs() <= f64::from(MAX_QP_OFFSET) {
        RateAdjustment::QpOffset(i8::try_from(saturating_i32(offset)).unwrap_or(MAX_QP_OFFSET))
    } else {
        RateAdjustment::Reconfigure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_changes_keep_the_encoder_as_is() {
        assert_eq!(
            plan_rate_change(10_000_000, 9_700_000, true),
            RateAdjustment::Keep
        );
    }

    #[test]
    fn moderate_drops_shift_qp_up() {
        assert_eq!(
            plan_rate_change(10_000_000, 5_000_000, true),
            RateAdjustment::QpOffset(6)
        );
        assert_eq!(
            plan_rate_change(10_000_000, 12_600_000, true),
            RateAdjustment::QpOffset(-2)
        );
    }

    #[test]
    fn large_changes_reconfigure() {
        assert_eq!(
            plan_rate_change(10_000_000, 2_000_000, true),
            RateAdjustment::Reconfigure
        );
    }

    #[test]
    fn without_roi_every_real_change_reconfigures() {
        assert_eq!(
            plan_rate_change(10_000_000, 8_000_000, false),
            RateAdjustment::Reconfigure
        );
        assert_eq!(plan_rate_change(0, 8_000_000, false), RateAdjustment::Keep);
    }
}
