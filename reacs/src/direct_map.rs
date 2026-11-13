use reacs_derive::Hashed;
use serde::{Deserialize, Serialize};

use crate::bit_set::BitSet;

#[derive(Default, Hashed, Serialize, Deserialize)]
pub struct DirectMap<T: Copy + Default> {
    pub active: BitSet,
    pub values: Vec<T>,
}

impl<T: Copy + Default> DirectMap<T> {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn has(&self, key: usize) -> bool {
        self.active.get(key)
    }

    #[inline]
    pub fn get(&self, key: usize) -> Option<&T> {
        self.has(key).then(|| &self.values[key])
    }

    #[inline]
    pub fn get_mut(&mut self, key: usize) -> Option<&mut T> {
        self.has(key).then(|| &mut self.values[key])
    }

    #[inline]
    pub fn insert(&mut self, key: usize, value: T) {
        self.active.set(key, true);
        let len = self.values.len();
        if key >= len {
            let new_size = (key + 1).next_power_of_two();
            self.values.resize(new_size, T::default());
        }

        self.values[key] = value;
    }

    #[inline]
    pub fn remove(&mut self, key: usize) {
        self.active.set(key, false);
    }

    #[inline]
    pub fn clear(&mut self) {
        self.active.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_map_has_nothing() {
        let mut map = DirectMap::<u32>::new();
        assert!(!map.has(0));
        assert_eq!(map.get(0), None);
        assert_eq!(map.get_mut(1_000), None);
    }

    #[test]
    fn insert_then_get() {
        let mut map = DirectMap::new();
        map.insert(0, 10u32);
        map.insert(70, 20);
        assert!(map.has(0));
        assert_eq!(map.get(0), Some(&10));
        assert_eq!(map.get(70), Some(&20));
        assert_eq!(map.get(1), None);
        assert_eq!(map.get(69), None);
        assert_eq!(map.get(71), None);
    }

    #[test]
    fn insert_overwrites() {
        let mut map = DirectMap::new();
        map.insert(5, 1u32);
        map.insert(5, 2);
        assert_eq!(map.get(5), Some(&2));
    }

    #[test]
    fn get_mut_changes_the_value() {
        let mut map = DirectMap::new();
        map.insert(5, 1u32);
        *map.get_mut(5).unwrap() = 9;
        assert_eq!(map.get(5), Some(&9));
    }

    #[test]
    fn values_grow_to_a_power_of_two() {
        let mut map = DirectMap::new();
        for (key, len) in [(0, 1), (1, 2), (3, 4), (4, 8), (8, 16), (100, 128)] {
            map.insert(key, 1u32);
            assert_eq!(map.values.len(), len);
        }
    }

    #[test]
    fn growing_keeps_existing_values() {
        let mut map = DirectMap::new();
        map.insert(1, 11u32);
        map.insert(1_000, 22);
        assert_eq!(map.get(1), Some(&11));
        assert_eq!(map.get(1_000), Some(&22));
        assert_eq!(map.get(2), None);
    }

    #[test]
    fn inserting_a_lower_key_keeps_higher_values() {
        let mut map = DirectMap::new();
        map.insert(7, 1u32);
        map.insert(2, 2);
        assert_eq!(map.get(7), Some(&1));
        assert_eq!(map.get(2), Some(&2));
    }

    #[test]
    fn remove_hides_only_that_key() {
        let mut map = DirectMap::new();
        map.insert(1, 1u32);
        map.insert(2, 2);
        map.remove(1);
        assert!(!map.has(1));
        assert_eq!(map.get(1), None);
        assert_eq!(map.get_mut(1), None);
        assert_eq!(map.get(2), Some(&2));
    }

    #[test]
    fn remove_of_an_unknown_key_does_nothing() {
        let mut map = DirectMap::<u32>::new();
        map.remove(1_000);
        assert!(map.active.words.is_empty());
        assert!(map.values.is_empty());
    }

    #[test]
    fn insert_after_remove_uses_the_new_value() {
        let mut map = DirectMap::new();
        map.insert(1, 1u32);
        map.remove(1);
        map.insert(1, 2);
        assert_eq!(map.get(1), Some(&2));
    }

    #[test]
    fn clear_hides_every_key() {
        let mut map = DirectMap::new();
        map.insert(1, 1u32);
        map.insert(100, 2);
        map.clear();
        assert!(!map.has(1));
        assert!(!map.has(100));

        map.insert(1, 3);
        assert_eq!(map.get(1), Some(&3));
        assert_eq!(map.get(100), None);
    }

    #[test]
    fn serialization_round_trip_keeps_active_keys() {
        let mut map = DirectMap::new();
        map.insert(1, 11u32);
        map.insert(70, 22);
        map.insert(3, 33);
        map.remove(3);

        let bytes = bincode::serialize(&map).unwrap();
        let restored: DirectMap<u32> = bincode::deserialize(&bytes).unwrap();

        assert_eq!(restored.get(1), Some(&11));
        assert_eq!(restored.get(70), Some(&22));
        assert_eq!(restored.get(3), None);
    }
}
