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

    /// Every layout worth trying, best first: each preferred format with its explicit
    /// modifiers, then with the implicit layout when the compositor accepts one. A driver may
    /// advertise a format its allocator cannot produce — Mesa's AMD GBM has no NV12 — so the
    /// caller falls through to the next candidate instead of giving up.
    pub(super) fn candidates(&self) -> Vec<ChosenLayout> {
        PREFERRED
            .into_iter()
            .filter_map(|fourcc| {
                let (_, modifiers) = self
                    .formats
                    .iter()
                    .find(|(known, _)| *known == fourcc.code())?;
                Some((fourcc, modifiers))
            })
            .flat_map(|(fourcc, modifiers)| {
                let explicit: Vec<u64> = modifiers
                    .iter()
                    .copied()
                    .filter(|modifier| *modifier != IMPLICIT_MODIFIER)
                    .collect();
                let implicit = modifiers.is_empty() || modifiers.contains(&IMPLICIT_MODIFIER);
                let with_explicit = (!explicit.is_empty()).then_some(ChosenLayout {
                    fourcc,
                    modifiers: explicit,
                });
                let with_implicit = implicit.then_some(ChosenLayout {
                    fourcc,
                    modifiers: Vec::new(),
                });
                with_explicit.into_iter().chain(with_implicit)
            })
            .collect()
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
            constraints.candidates().first(),
            Some(&ChosenLayout {
                fourcc: DrmFourcc::Nv12,
                modifiers: vec![0x0200_0000_0000_0001],
            })
        );
    }

    #[test]
    fn every_offered_layout_is_a_fallback_in_preference_order() {
        let mut constraints = BufferConstraints::default();
        constraints.begin(1920, 1200);
        constraints.add_format(DrmFourcc::Nv12.code());
        constraints.add_modifier(DrmFourcc::Nv12.code(), 0x00ff_ffff, 0xffff_ffff);
        constraints.add_format(DrmFourcc::Xrgb8888.code());
        constraints.add_modifier(DrmFourcc::Xrgb8888.code(), 0x0200_0000, 0x0000_0001);
        constraints.add_modifier(DrmFourcc::Xrgb8888.code(), 0x00ff_ffff, 0xffff_ffff);
        let layouts: Vec<(DrmFourcc, usize)> = constraints
            .candidates()
            .iter()
            .map(|layout| (layout.fourcc, layout.modifiers.len()))
            .collect();
        assert_eq!(
            layouts,
            vec![
                (DrmFourcc::Nv12, 0),
                (DrmFourcc::Xrgb8888, 1),
                (DrmFourcc::Xrgb8888, 0),
            ]
        );
    }

    #[test]
    fn the_implicit_modifier_is_not_passed_to_the_allocator() {
        let mut constraints = BufferConstraints::default();
        constraints.begin(640, 480);
        constraints.add_format(DrmFourcc::Xrgb8888.code());
        constraints.add_modifier(DrmFourcc::Xrgb8888.code(), 0x00ff_ffff, 0xffff_ffff);
        assert_eq!(
            constraints
                .candidates()
                .first()
                .map(|layout| layout.modifiers.len()),
            Some(0)
        );
    }

    #[test]
    fn formats_the_encoder_cannot_take_are_ignored() {
        let mut constraints = BufferConstraints::default();
        constraints.begin(640, 480);
        constraints.add_format(ARGB8888);
        assert!(constraints.candidates().is_empty());
    }

    #[test]
    fn a_new_sequence_replaces_the_old_one() {
        let mut constraints = BufferConstraints::default();
        constraints.begin(640, 480);
        constraints.add_format(DrmFourcc::Nv12.code());
        constraints.begin(800, 600);
        assert_eq!((constraints.width, constraints.height), (800, 600));
        assert!(constraints.candidates().is_empty());
    }
}
