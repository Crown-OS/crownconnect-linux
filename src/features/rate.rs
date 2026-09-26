/// The encoder rate congestion control allows: its target bitrate, capped by the per-frame byte
/// budget the session sets to close the loop within a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RatePlanner {
    target_bps: u32,
    budget_bps: Option<u32>,
    frames_per_second: u32,
}

const BITS_PER_BYTE: u32 = 8;

impl RatePlanner {
    pub(crate) const fn new(target_bps: u32, frames_per_second: u32) -> Self {
        Self {
            target_bps,
            budget_bps: None,
            frames_per_second,
        }
    }

    pub(crate) const fn bitrate(&self) -> u32 {
        match self.budget_bps {
            Some(budget) if budget < self.target_bps => budget,
            _ => self.target_bps,
        }
    }

    /// The new encoder bitrate, when it changed.
    pub(crate) fn set_target(&mut self, bits_per_second: u32) -> Option<u32> {
        self.update(|planner| planner.target_bps = bits_per_second)
    }

    pub(crate) fn set_frame_budget(&mut self, bytes: u32) -> Option<u32> {
        let per_second = bytes
            .saturating_mul(BITS_PER_BYTE)
            .saturating_mul(self.frames_per_second.max(1));
        self.update(|planner| planner.budget_bps = Some(per_second))
    }

    fn update(&mut self, change: impl FnOnce(&mut Self)) -> Option<u32> {
        let before = self.bitrate();
        change(self);
        let after = self.bitrate();
        (after != before && after > 0).then_some(after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_budget_caps_the_target() {
        let mut planner = RatePlanner::new(20_000_000, 60);
        assert_eq!(planner.set_frame_budget(20_000), Some(9_600_000));
        assert_eq!(planner.set_target(8_000_000), Some(8_000_000));
        assert_eq!(planner.set_frame_budget(20_000), None);
        assert_eq!(planner.set_target(30_000_000), Some(9_600_000));
    }
}
