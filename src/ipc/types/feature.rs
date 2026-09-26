use serde::{Deserialize, Serialize};

/// One capability a paired device can share with this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Feature {
    Mirror,
    Camera,
    Mic,
    Monitor,
    Unicursor,
    Calls,
    Clipboard,
    Notifications,
    Battery,
    Hotspot,
    Files,
}

impl Feature {
    pub const ALL: [Self; 11] = [
        Self::Mirror,
        Self::Camera,
        Self::Mic,
        Self::Monitor,
        Self::Unicursor,
        Self::Calls,
        Self::Clipboard,
        Self::Notifications,
        Self::Battery,
        Self::Hotspot,
        Self::Files,
    ];

    const fn bit(self) -> u32 {
        1 << self as u8
    }
}

/// Where one feature of one device stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeatureState {
    Disabled,
    /// Allowed but idle.
    Enabled,
    /// Allowed and running right now.
    Active,
}

/// A set of [`Feature`]s packed into one word.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FeatureSet(u32);

impl FeatureSet {
    pub const EMPTY: Self = Self(0);

    #[must_use]
    pub const fn contains(self, feature: Feature) -> bool {
        self.0 & feature.bit() != 0
    }

    #[must_use]
    pub const fn with(self, feature: Feature) -> Self {
        Self(self.0 | feature.bit())
    }

    #[must_use]
    pub const fn without(self, feature: Feature) -> Self {
        Self(self.0 & !feature.bit())
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Feature> {
        Feature::ALL
            .into_iter()
            .filter(move |feature| self.contains(*feature))
    }
}

impl FromIterator<Feature> for FeatureSet {
    fn from_iter<I: IntoIterator<Item = Feature>>(features: I) -> Self {
        features.into_iter().fold(Self::EMPTY, Self::with)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_feature_owns_a_distinct_bit() {
        let all: FeatureSet = Feature::ALL.into_iter().collect();
        assert_eq!(all.iter().count(), Feature::ALL.len());
    }

    #[test]
    fn with_and_without_toggle_only_their_feature() {
        let set = FeatureSet::EMPTY.with(Feature::Mirror).with(Feature::Files);
        assert!(set.contains(Feature::Mirror));
        assert!(!set.contains(Feature::Camera));
        let set = set.without(Feature::Mirror);
        assert_eq!(set.iter().collect::<Vec<_>>(), [Feature::Files]);
    }
}
