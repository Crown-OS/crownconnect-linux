/// A damaged rectangle in buffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl DamageRect {
    fn union(self, other: Self) -> Self {
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = self
            .x
            .saturating_add(self.width)
            .max(other.x.saturating_add(other.width));
        let bottom = self
            .y
            .saturating_add(self.height)
            .max(other.y.saturating_add(other.height));
        Self {
            x: left,
            y: top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        }
    }
}

/// Encoders use damage only as a hint, so past this many rectangles they collapse into one.
const MAX_DAMAGE_RECTS: usize = 16;

/// Damage collected between two frame events.
#[derive(Debug, Default)]
pub(super) struct DamageCollector {
    rects: Vec<DamageRect>,
}

impl DamageCollector {
    pub(super) fn add(&mut self, rect: DamageRect) {
        if rect.width <= 0 || rect.height <= 0 {
            return;
        }
        if self.rects.len() < MAX_DAMAGE_RECTS {
            self.rects.push(rect);
            return;
        }
        let merged = self.rects.drain(..).fold(rect, DamageRect::union);
        self.rects.push(merged);
    }

    pub(super) fn take(&mut self) -> Vec<DamageRect> {
        std::mem::take(&mut self.rects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rect(x: i32, y: i32, width: i32, height: i32) -> DamageRect {
        DamageRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn rectangles_are_kept_until_the_cap_then_merged() {
        let mut damage = DamageCollector::default();
        for index in 0..16 {
            damage.add(rect(index * 10, 0, 5, 5));
        }
        damage.add(rect(0, 100, 1, 1));
        assert_eq!(damage.take(), [rect(0, 0, 155, 101)]);
        assert!(damage.take().is_empty());
    }

    #[test]
    fn empty_rectangles_are_ignored() {
        let mut damage = DamageCollector::default();
        damage.add(rect(3, 3, 0, 10));
        damage.add(rect(3, 3, 10, -1));
        assert!(damage.take().is_empty());
    }
}
