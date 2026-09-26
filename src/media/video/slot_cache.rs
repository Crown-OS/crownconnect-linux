use crate::media::MediaError;

/// The deepest capture ring the encoder keeps mappings for.
pub const MAX_RING_SLOTS: usize = 4;

/// Per-ring-slot cache of expensive mappings, such as a dmabuf imported as a VA surface.
/// A slot is remapped only when the buffer behind it changes.
#[derive(Debug)]
pub struct SlotCache<K, V> {
    slots: [Option<(K, V)>; MAX_RING_SLOTS],
}

impl<K, V> Default for SlotCache<K, V> {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
        }
    }
}

impl<K: PartialEq, V> SlotCache<K, V> {
    pub fn get_or_map(
        &mut self,
        slot: usize,
        key: K,
        map: impl FnOnce() -> Result<V, MediaError>,
    ) -> Result<&mut V, MediaError> {
        let entry = self.slots.get_mut(slot).ok_or(MediaError::InvalidFrame(
            "ring slot index exceeds MAX_RING_SLOTS",
        ))?;
        if entry.as_ref().is_none_or(|(cached, _)| *cached != key) {
            *entry = None;
            *entry = Some((key, map()?));
        }
        entry
            .as_mut()
            .map(|(_, value)| value)
            .ok_or(MediaError::InvalidFrame("ring slot mapping missing"))
    }

    pub fn clear(&mut self) {
        self.slots.iter_mut().for_each(|slot| *slot = None);
    }

    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn counting_map<'a>(
        calls: &'a Cell<u32>,
        value: &'a str,
    ) -> impl FnOnce() -> Result<String, MediaError> + 'a {
        move || {
            calls.set(calls.get() + 1);
            Ok(value.to_owned())
        }
    }

    #[test]
    fn maps_each_slot_once_while_the_buffer_is_unchanged() -> Result<(), MediaError> {
        let calls = Cell::new(0);
        let mut cache = SlotCache::default();
        for _ in 0..3 {
            for slot in 0..MAX_RING_SLOTS {
                cache.get_or_map(slot, slot, counting_map(&calls, "surface"))?;
            }
        }
        assert_eq!(calls.get(), 4);
        assert_eq!(cache.len(), MAX_RING_SLOTS);
        Ok(())
    }

    #[test]
    fn remaps_a_slot_when_its_buffer_changes() -> Result<(), MediaError> {
        let calls = Cell::new(0);
        let mut cache = SlotCache::default();
        cache.get_or_map(1, "buffer-a", counting_map(&calls, "a"))?;
        let value = cache.get_or_map(1, "buffer-b", counting_map(&calls, "b"))?;
        assert_eq!(value, "b");
        assert_eq!(calls.get(), 2);
        assert_eq!(cache.len(), 1);
        Ok(())
    }

    #[test]
    fn rejects_slots_beyond_the_ring() {
        let mut cache: SlotCache<u8, u8> = SlotCache::default();
        assert!(cache.get_or_map(MAX_RING_SLOTS, 0, || Ok(0)).is_err());
        assert!(cache.is_empty());
    }

    #[test]
    fn failed_mapping_leaves_the_slot_empty() {
        let mut cache: SlotCache<u8, u8> = SlotCache::default();
        let failed = cache.get_or_map(0, 1, || Err(MediaError::Unsupported("test")));
        assert!(failed.is_err());
        assert!(cache.is_empty());
    }

    #[test]
    fn clear_drops_every_mapping() -> Result<(), MediaError> {
        let mut cache = SlotCache::default();
        cache.get_or_map(0, 0, || Ok(()))?;
        cache.get_or_map(3, 3, || Ok(()))?;
        cache.clear();
        assert!(cache.is_empty());
        Ok(())
    }
}
