//! [`IndexSet`], the set that iterates in insertion order and keeps it through saves.

use std::{fmt, hash::Hash, marker::PhantomData, slice};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error as _, SeqAccess, Visitor},
    ser::SerializeSeq,
};

use crate::index_map::{KeyOf, Table};

const MAX_PREALLOCATED: usize = 4096;

/// A set that iterates in insertion order, the same on every run.
///
/// Use it instead of `std::collections::HashSet` in resources, for the reason
/// [`IndexMap`](crate::index_map::IndexMap) replaces `HashMap`: a `HashSet` iterates in an order
/// that changes after a save and load. It works like an `IndexMap<T, ()>` with a set's API:
/// [`insert`](Self::insert) and [`swap_remove`](Self::swap_remove) answer `true` or `false`, and
/// the values come out as a plain `&[T]`. It saves the same bytes as that map.
///
/// Values live in one dense list, so iterating is as fast as iterating a slice. Every operation
/// costs about the same at any size; removing a value moves the last one into its place
/// ([`swap_remove`](Self::swap_remove)), so it's O(1) but changes the order.
///
/// # Examples
///
/// ```
/// use reacs::prelude::*;
///
/// let mut selected = IndexSet::new();
/// assert!(selected.insert("archer"));
/// assert!(selected.insert("knight"));
/// assert!(selected.insert("mage"));
/// assert!(!selected.insert("archer"));
///
/// assert!(selected.swap_remove(&"archer"));
/// assert_eq!(selected.as_slice(), ["mage", "knight"]);
/// ```
pub struct IndexSet<T> {
    table: Table<T, SetKey>,
}

pub(crate) struct SetKey;

impl<T: Hash + Eq> KeyOf<T> for SetKey {
    type Key = T;
    const NAME: &'static str = "IndexSet";

    #[inline(always)]
    fn key(entry: &T) -> &T {
        entry
    }
}

impl<T: Clone> Clone for IndexSet<T> {
    fn clone(&self) -> Self {
        Self {
            table: self.table.clone(),
        }
    }
}

impl<T> Default for IndexSet<T> {
    fn default() -> Self {
        Self {
            table: Table::default(),
        }
    }
}

impl<T: Hash + Eq> IndexSet<T> {
    /// Creates an empty set. It allocates nothing until the first insert.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty set with room for `capacity` values before it grows.
    pub fn with_capacity(capacity: usize) -> Self {
        let mut set = Self::new();
        set.reserve(capacity);
        set
    }

    /// Makes room for at least `additional` more values.
    pub fn reserve(&mut self, additional: usize) {
        self.table.reserve(additional);
    }

    /// Returns the number of values.
    #[inline]
    pub fn len(&self) -> usize {
        self.table.entries().len()
    }

    /// Returns `true` if the set has no values.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.table.entries().is_empty()
    }

    /// Returns every value as a slice, in order.
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        self.table.entries()
    }

    /// Returns an iterator over the values, in order.
    #[inline]
    pub fn iter(&self) -> slice::Iter<'_, T> {
        self.table.entries().iter()
    }

    /// Removes every value, keeping the allocated memory.
    pub fn clear(&mut self) {
        self.table.clear();
    }

    /// Returns `true` if the set holds `value`.
    #[inline]
    pub fn contains(&self, value: &T) -> bool {
        self.table.index_of(value).is_some()
    }

    /// Adds `value` at the end of the order, and returns `true` if it wasn't in the set.
    ///
    /// A value already in the set keeps its position, and the set keeps the stored value.
    ///
    /// # Panics
    ///
    /// Panics if the set already holds `u32::MAX` values and `value` is new.
    pub fn insert(&mut self, value: T) -> bool {
        self.table.insert(value).is_none()
    }

    /// Removes `value`, and returns `true` if it was in the set.
    ///
    /// The last value moves into the removed one's position, so this is O(1) but changes the
    /// order. Code that relies on insertion order after removals must sort the values itself.
    pub fn swap_remove(&mut self, value: &T) -> bool {
        self.table.swap_remove(value).is_some()
    }

    #[cfg(any(test, feature = "invariants"))]
    pub(crate) fn check_invariants(&self) {
        self.table.check_invariants();
    }
}

impl<'a, T> IntoIterator for &'a IndexSet<T> {
    type Item = &'a T;
    type IntoIter = slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.table.entries().iter()
    }
}

impl<T: fmt::Debug> fmt::Debug for IndexSet<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.table.entries()).finish()
    }
}

impl<T: Hash + Eq> Extend<T> for IndexSet<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        self.reserve(iter.size_hint().0);
        for value in iter {
            self.insert(value);
        }
    }
}

impl<T: Hash + Eq> FromIterator<T> for IndexSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut set = Self::new();
        set.extend(iter);
        set
    }
}

impl<T: Hash + Eq, const N: usize> From<[T; N]> for IndexSet<T> {
    fn from(values: [T; N]) -> Self {
        Self::from_iter(values)
    }
}

impl<T: Serialize> Serialize for IndexSet<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let values = self.table.entries();
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for value in values {
            seq.serialize_element(value)?;
        }
        seq.end()
    }
}

impl<'de, T: Deserialize<'de> + Hash + Eq> Deserialize<'de> for IndexSet<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(IndexSetVisitor(PhantomData))
    }
}

struct IndexSetVisitor<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de> + Hash + Eq> Visitor<'de> for IndexSetVisitor<T> {
    type Value = IndexSet<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a sequence")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let hint = access.size_hint().unwrap_or(0).min(MAX_PREALLOCATED);
        let mut set = IndexSet::with_capacity(hint);
        while let Some(value) = access.next_element()? {
            if !set.insert(value) {
                return Err(A::Error::custom("duplicate value in IndexSet"));
            }
        }
        #[cfg(any(test, feature = "invariants"))]
        set.check_invariants();
        Ok(set)
    }
}

/// Saves the values in order, by their own rule (one memory copy when they're plain), and rebuilds
/// the table on load at its final size: the table is never saved.
impl<T: crate::save::Save + Hash + Eq> crate::save::Save for IndexSet<T> {
    const PLAIN: bool = false;

    fn save(&self, out: &mut Vec<u8>) {
        T::save_slice(self.table.entries(), out);
    }

    fn load(reader: &mut crate::save::Reader<'_>) -> Self {
        let values = T::load_vec(reader);
        let mut set = Self::with_capacity(values.len());
        for value in values {
            if !set.insert(value) {
                panic!("Failed to load save, it's corrupted: duplicate value in IndexSet");
            }
        }
        #[cfg(any(test, feature = "invariants"))]
        set.check_invariants();
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_map::IndexMap;
    use crate::save::Save;
    use std::hash::Hasher;

    #[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, crate::save::Save)]
    struct Weak(u32);

    impl Hash for Weak {
        fn hash<H: Hasher>(&self, state: &mut H) {
            state.write_u32(self.0 % 3);
        }
    }

    struct Xorshift(u64);

    impl Xorshift {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    fn check_against_model<T: Hash + Eq + Copy + fmt::Debug>(keys: u32, key: impl Fn(u32) -> T) {
        let mut rng = Xorshift(0x9e37_79b9);
        let mut set = IndexSet::new();
        let mut model: Vec<T> = Vec::new();
        for _ in 0..crate::test_size(20_000, 400) {
            let value = key(rng.below(keys as u64) as u32);
            let position = model.iter().position(|v| *v == value);
            match rng.below(1000) {
                0..=599 => {
                    assert_eq!(set.insert(value), position.is_none());
                    if position.is_none() {
                        model.push(value);
                    }
                }
                600..=997 => {
                    assert_eq!(set.swap_remove(&value), position.is_some());
                    if let Some(index) = position {
                        model.swap_remove(index);
                    }
                }
                _ => {
                    set.clear();
                    model.clear();
                }
            }
            set.check_invariants();
            assert_eq!(set.as_slice(), model.as_slice());
        }
        for k in 0..keys {
            assert_eq!(set.contains(&key(k)), model.contains(&key(k)));
        }
    }

    #[test]
    fn matches_a_vec_model() {
        check_against_model(300, |k| k as u64);
    }

    #[test]
    fn matches_a_vec_model_with_colliding_hashes() {
        check_against_model(60, Weak);
    }

    #[test]
    fn insert_keeps_the_first_position() {
        let mut set = IndexSet::from([3u32, 1, 2]);
        assert!(!set.insert(3));
        assert_eq!(set.as_slice(), [3, 1, 2]);
    }

    #[test]
    fn swap_remove_moves_the_last_value() {
        let mut set: IndexSet<u32> = (0..5).collect();
        assert!(set.swap_remove(&1));
        assert!(!set.swap_remove(&1));
        assert_eq!(set.as_slice(), [0, 4, 2, 3]);
        assert!(set.contains(&4));
    }

    #[test]
    fn empty_set_finds_nothing() {
        let mut set = IndexSet::<u64>::new();
        assert!(!set.contains(&7));
        assert!(!set.swap_remove(&7));
        assert!(set.is_empty());
    }

    #[test]
    fn save_trait_keeps_order_and_bytes() {
        let mut set: IndexSet<Weak> = (0..40).map(Weak).collect();
        for k in (0..40).step_by(3) {
            set.swap_remove(&Weak(k));
        }
        let mut bytes = Vec::new();
        set.save(&mut bytes);

        let loaded = IndexSet::<Weak>::load(&mut crate::save::Reader::new(&bytes));
        assert_eq!(loaded.as_slice(), set.as_slice());
        let mut again = Vec::new();
        loaded.save(&mut again);
        assert_eq!(again, bytes);
    }

    #[test]
    fn saves_like_a_map_of_units() {
        let set: IndexSet<u64> = [9, 4, 7].into();
        let map: IndexMap<u64, ()> = [(9, ()), (4, ()), (7, ())].into();
        let (mut set_bytes, mut map_bytes) = (Vec::new(), Vec::new());
        set.save(&mut set_bytes);
        map.save(&mut map_bytes);
        assert_eq!(set_bytes, map_bytes);
    }

    #[test]
    #[should_panic(expected = "duplicate value in IndexSet")]
    fn save_trait_load_rejects_duplicates() {
        let mut bytes = Vec::new();
        [5u32, 5].as_slice().to_vec().save(&mut bytes);
        IndexSet::<u32>::load(&mut crate::save::Reader::new(&bytes));
    }

    #[test]
    fn serde_keeps_order_and_rejects_duplicates() {
        let set: IndexSet<u32> = [8, 2, 5].into();
        let bytes = postcard::to_stdvec(&set).unwrap();
        let loaded: IndexSet<u32> = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(loaded.as_slice(), [8, 2, 5]);

        let duplicate = postcard::to_stdvec(&[1u32, 1].as_slice()).unwrap();
        assert!(postcard::from_bytes::<IndexSet<u32>>(&duplicate).is_err());
    }

    #[test]
    fn debug_shows_values_in_order() {
        let set: IndexSet<u32> = [3, 1, 2].into();
        assert_eq!(format!("{set:?}"), "{3, 1, 2}");
    }
}
