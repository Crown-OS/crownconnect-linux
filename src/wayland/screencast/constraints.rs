use crate::media::video::DrmFourcc;

/// Tells the compositor to pick the layout itself; a buffer allocated without explicit
/// modifiers is described with it.
pub(super) const IMPLICIT_MODIFIER: u64 = 0x00ff_ffff_ffff_ffff;

/// Formats in the order the encoder prefers them: NV12 needs no conversion on the way in.
const PREFERRED: [DrmFourcc; 2] = [DrmFourcc::Nv12, DrmFourcc::Xrgb8888];

/// One complete constraints sequence from the compositor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct BufferConstraints {
    pub(super) width: u32,
    pub(super) height: u32,
    formats: Vec<(u32, Vec<u64>)>,
}

/// The format and modifiers the ring is allocated with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChosenLayout {
    pub(super) fourcc: DrmFourcc,
    /// Explicit modifiers the compositor accepts; empty when only the implicit one is offered.
    pub(super) modifiers: Vec<u64>,
}

impl BufferConstraints {
    pub(super) fn begin(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.formats.clear();
    }

    pub(super) fn add_format(&mut self, fourcc: u32) {
        if !self.formats.iter().any(|(known, _)| *known == fourcc) {
            self.formats.push((fourcc, Vec::new()));
        }
    }

    pub(super) fn add_modifier(&mut self, fourcc: u32, modifier_hi: u32, modifier_lo: u32) {
        let modifier = (u64::from(modifier_hi) << 32) | u64::from(modifier_lo);
        if let Some((_, modifiers)) = self.formats.iter_mut().find(|(known, _)| *known == fourcc) {
            modifiers.push(modifier);
        }
    }

    pub(super) fn choose(&self) -> Option<ChosenLayout> {
        PREFERRED.into_iter().find_map(|fourcc| {
            let (_, modifiers) = self
                .formats
                .iter()
                .find(|(known, _)| *known == fourcc.code())?;
            Some(ChosenLayout {
                fourcc,
                modifiers: modifiers
                    .iter()
                    .copied()
                    .filter(|modifier| *modifier != IMPLICIT_MODIFIER)
                    .collect(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARGB8888: u32 = u32::from_le_bytes(*b"AR24");

    #[test]
    fn nv12_wins_over_rgb_when_both_are_offered() {
        let mut constraints = BufferConstraints::default();
        constraints.begin(1920, 1080);
        constraints.add_format(DrmFourcc::Xrgb8888.code());
        constraints.add_modifier(DrmFourcc::Xrgb8888.code(), 0, 0);
        constraints.add_format(DrmFourcc::Nv12.code());
        constraints.add_modifier(DrmFourcc::Nv12.code(), 0x0200_0000, 0x0000_0001);
        assert_eq!(
            constraints.choose(),
            Some(ChosenLayout {
                fourcc: DrmFourcc::Nv12,
                modifiers: vec![0x0200_0000_0000_0001],
            })
        );
    }

    #[test]
    fn the_implicit_modifier_is_not_passed_to_the_allocator() {
        let mut constraints = BufferConstraints::default();
        constraints.begin(640, 480);
        constraints.add_format(DrmFourcc::Xrgb8888.code());
        constraints.add_modifier(DrmFourcc::Xrgb8888.code(), 0x00ff_ffff, 0xffff_ffff);
        assert_eq!(
            constraints.choose().map(|layout| layout.modifiers),
            Some(Vec::new())
        );
    }

    #[test]
    fn formats_the_encoder_cannot_take_are_ignored() {
        let mut constraints = BufferConstraints::default();
        constraints.begin(640, 480);
        constraints.add_format(ARGB8888);
        assert_eq!(constraints.choose(), None);
    }

    #[test]
    fn a_new_sequence_replaces_the_old_one() {
        let mut constraints = BufferConstraints::default();
        constraints.begin(640, 480);
        constraints.add_format(DrmFourcc::Nv12.code());
        constraints.begin(800, 600);
        assert_eq!((constraints.width, constraints.height), (800, 600));
        assert_eq!(constraints.choose(), None);
    }
}
