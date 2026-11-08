use std::{
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem,
    ops::Index,
    slice,
};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error as _, MapAccess, Visitor},
    ser::SerializeMap,
};

const EMPTY: u32 = u32::MAX;
const MIN_SLOTS: usize = 16;
const LOAD_SHIFT: u32 = 2;
const MAX_PREALLOCATED: usize = 4096;

#[inline(always)]
fn folded_multiply(a: u64, b: u64) -> u64 {
    let full = a as u128 * b as u128;
    full as u64 ^ (full >> 64) as u64
}

struct FixedHasher(u64);

impl Hasher for FixedHasher {
    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.0 = folded_multiply(self.0 ^ value, 0x9e37_79b9_7f4a_7c15);
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.write_u64(value as u64);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn finish(&self) -> u64 {
        folded_multiply(self.0, 0x082e_fa98_ec4e_6c89)
    }
}

#[inline]
fn tag_of<K: Hash>(key: &K) -> u32 {
    let mut hasher = FixedHasher(0x243f_6a88_85a3_08d3);
    key.hash(&mut hasher);
    hasher.finish() as u32
}

#[derive(Clone, Copy)]
struct Slot {
    index: u32,
    tag: u32,
}

impl Slot {
    #[inline(always)]
    fn new(index: usize, tag: u32) -> Self {
        Self {
            index: index as u32,
            tag,
        }
    }
}

const EMPTY_SLOT: Slot = Slot {
    index: EMPTY,
    tag: 0,
};

#[derive(Clone)]
pub struct IndexMap<K, V> {
    entries: Vec<(K, V)>,
    slots: Vec<Slot>,
}

impl<K, V> Default for IndexMap<K, V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            slots: Vec::new(),
        }
    }
}

impl<K: Hash + Eq, V> IndexMap<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let mut map = Self::new();
        map.reserve(capacity);
        map
    }

    pub fn reserve(&mut self, additional: usize) {
        self.entries.reserve(additional);
        self.grow_for(self.entries.len() + additional);
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn as_slice(&self) -> &[(K, V)] {
        &self.entries
    }

    #[inline]
    pub fn iter(&self) -> slice::Iter<'_, (K, V)> {
        self.entries.iter()
    }

    #[inline]
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.entries.iter_mut().map(|(key, value)| (&*key, value))
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.slots.fill(EMPTY_SLOT);
    }

    #[inline]
    pub fn contains_key(&self, key: &K) -> bool {
        self.index_of(key).is_some()
    }

    #[inline]
    pub fn get(&self, key: &K) -> Option<&V> {
        let index = self.index_of(key)?;
        Some(&self.entries[index].1)
    }

    #[inline]
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let index = self.index_of(key)?;
        Some(&mut self.entries[index].1)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.grow_for(self.entries.len() + 1);
        let tag = tag_of(&key);
        match self.probe(tag, &key) {
            Ok(pos) => {
                let index = self.slot(pos).index as usize;
                Some(mem::replace(&mut self.entries[index].1, value))
            }
            Err(pos) => {
                let index = self.entries.len();
                assert!(index < EMPTY as usize, "IndexMap can't hold more entries");
                self.entries.push((key, value));
                self.set_slot(pos, Slot::new(index, tag));
                None
            }
        }
    }

    pub fn swap_remove(&mut self, key: &K) -> Option<V> {
        let pos = self.find(key)?;
        let index = self.slot(pos).index as usize;
        let last = self.entries.len() - 1;
        if index != last {
            let tag = tag_of(&self.entries[last].0);
            let moved = self.slot_of_index(tag, last);
            self.set_slot(moved, Slot::new(index, tag));
        }
        self.erase_slot(pos);
        Some(self.entries.swap_remove(index).1)
    }

    #[inline]
    fn index_of(&self, key: &K) -> Option<usize> {
        self.find(key).map(|pos| self.slot(pos).index as usize)
    }

    #[inline]
    fn find(&self, key: &K) -> Option<usize> {
        if self.entries.is_empty() {
            return None;
        }
        self.probe(tag_of(key), key).ok()
    }

    #[inline]
    fn probe(&self, tag: u32, key: &K) -> Result<usize, usize> {
        let mask = self.mask();
        let mut pos = tag as usize & mask;
        loop {
            let slot = self.slot(pos);
            if slot.index == EMPTY {
                return Err(pos);
            }
            if slot.tag == tag
                && unsafe { &self.entries.get_unchecked(slot.index as usize).0 } == key
            {
                return Ok(pos);
            }
            pos = (pos + 1) & mask;
        }
    }

    #[inline(always)]
    fn mask(&self) -> usize {
        self.slots.len() - 1
    }

    #[inline(always)]
    fn slot(&self, pos: usize) -> Slot {
        unsafe { *self.slots.get_unchecked(pos) }
    }

    #[inline(always)]
    fn set_slot(&mut self, pos: usize, slot: Slot) {
        unsafe { *self.slots.get_unchecked_mut(pos) = slot }
    }

    fn first_empty(&self, tag: u32) -> usize {
        let mask = self.mask();
        let mut pos = tag as usize & mask;
        while self.slot(pos).index != EMPTY {
            pos = (pos + 1) & mask;
        }
        pos
    }

    fn slot_of_index(&self, tag: u32, index: usize) -> usize {
        let mask = self.mask();
        let mut pos = tag as usize & mask;
        while self.slot(pos).index != index as u32 {
            pos = (pos + 1) & mask;
        }
        pos
    }

    fn grow_for(&mut self, needed: usize) {
        let wanted = needed
            .checked_mul(1 << LOAD_SHIFT)
            .expect("IndexMap capacity overflow");
        if wanted <= self.slots.len() {
            return;
        }
        let slots = vec![EMPTY_SLOT; wanted.next_power_of_two().max(MIN_SLOTS)];
        for slot in mem::replace(&mut self.slots, slots) {
            if slot.index != EMPTY {
                let pos = self.first_empty(slot.tag);
                self.set_slot(pos, slot);
            }
        }
    }

    fn erase_slot(&mut self, pos: usize) {
        let mask = self.mask();
        let mut hole = pos;
        let mut next = (pos + 1) & mask;
        loop {
            let slot = self.slot(next);
            if slot.index == EMPTY {
                break;
            }
            let home = slot.tag as usize & mask;
            if next.wrapping_sub(home) & mask >= next.wrapping_sub(hole) & mask {
                self.set_slot(hole, slot);
                hole = next;
            }
            next = (next + 1) & mask;
        }
        self.set_slot(hole, EMPTY_SLOT);
    }
}

impl<K: Hash + Eq, V> Index<&K> for IndexMap<K, V> {
    type Output = V;

    fn index(&self, key: &K) -> &V {
        self.get(key).expect("key not found in IndexMap")
    }
}

impl<'a, K, V> IntoIterator for &'a IndexMap<K, V> {
    type Item = &'a (K, V);
    type IntoIter = slice::Iter<'a, (K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for IndexMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.entries.iter().map(|(key, value)| (key, value)))
            .finish()
    }
}

impl<K: Hash + Eq, V> Extend<(K, V)> for IndexMap<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        self.reserve(iter.size_hint().0);
        for (key, value) in iter {
            self.insert(key, value);
        }
    }
}

impl<K: Hash + Eq, V> FromIterator<(K, V)> for IndexMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        map.extend(iter);
        map
    }
}

impl<K: Hash + Eq, V, const N: usize> From<[(K, V); N]> for IndexMap<K, V> {
    fn from(entries: [(K, V); N]) -> Self {
        Self::from_iter(entries)
    }
}

impl<K: Serialize, V: Serialize> Serialize for IndexMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, value) in &self.entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de, K, V> Deserialize<'de> for IndexMap<K, V>
where
    K: Deserialize<'de> + Hash + Eq,
    V: Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(IndexMapVisitor(PhantomData))
    }
}

struct IndexMapVisitor<K, V>(PhantomData<(K, V)>);

impl<'de, K, V> Visitor<'de> for IndexMapVisitor<K, V>
where
    K: Deserialize<'de> + Hash + Eq,
    V: Deserialize<'de>,
{
    type Value = IndexMap<K, V>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a map")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let hint = access.size_hint().unwrap_or(0).min(MAX_PREALLOCATED);
        let mut map = IndexMap::with_capacity(hint);
        while let Some((key, value)) = access.next_entry()? {
            if map.insert(key, value).is_some() {
                return Err(A::Error::custom("duplicate key in IndexMap"));
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
    struct Weak(u32);

    impl Hash for Weak {
        fn hash<H: Hasher>(&self, state: &mut H) {
            state.write_u32(self.0 % 3);
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Op {
        Insert(u32, u64),
        Remove(u32),
        Clear,
    }

    fn ops(seed: u64, count: usize, keys: u32) -> Vec<Op> {
        let mut rng = Rng(seed);
        (0..count)
            .map(|_| match rng.below(1000) {
                0..=599 => Op::Insert(rng.below(keys as u64) as u32, rng.below(1 << 40)),
                600..=997 => Op::Remove(rng.below(keys as u64) as u32),
                _ => Op::Clear,
            })
            .collect()
    }

    fn apply<K: Hash + Eq + Copy>(map: &mut IndexMap<K, u64>, op: Op, key: impl Fn(u32) -> K) {
        match op {
            Op::Insert(k, v) => {
                map.insert(key(k), v);
            }
            Op::Remove(k) => {
                map.swap_remove(&key(k));
            }
            Op::Clear => map.clear(),
        }
    }

    fn apply_model<K: Eq + Copy>(model: &mut Vec<(K, u64)>, op: Op, key: impl Fn(u32) -> K) {
        let position = model.iter().position(|(k, _)| match op {
            Op::Insert(wanted, _) | Op::Remove(wanted) => *k == key(wanted),
            Op::Clear => false,
        });
        match (op, position) {
            (Op::Insert(_, v), Some(index)) => model[index].1 = v,
            (Op::Insert(k, v), None) => model.push((key(k), v)),
            (Op::Remove(_), Some(index)) => {
                model.swap_remove(index);
            }
            (Op::Remove(_), None) => {}
            (Op::Clear, _) => model.clear(),
        }
    }

    fn check_against_model<K: Hash + Eq + Copy + fmt::Debug>(
        keys: u32,
        key: impl Fn(u32) -> K + Copy,
    ) {
        let mut map = IndexMap::new();
        let mut model = Vec::new();
        for op in ops(0x9e37_79b9, 20_000, keys) {
            apply(&mut map, op, key);
            apply_model(&mut model, op, key);
            assert_eq!(map.as_slice(), model.as_slice(), "after {op:?}");
        }
        for k in 0..keys {
            let expected = model.iter().find(|(m, _)| *m == key(k)).map(|(_, v)| v);
            assert_eq!(map.get(&key(k)), expected);
        }
    }

    #[test]
    fn matches_a_vec_model() {
        check_against_model(300, |k| k as u64);
    }

    #[test]
    fn matches_a_vec_model_with_colliding_hashes() {
        check_against_model(300, Weak);
    }

    #[test]
    fn matches_a_vec_model_in_a_small_table() {
        check_against_model(12, Weak);
    }

    #[test]
    fn order_does_not_depend_on_capacity() {
        let mut small = IndexMap::new();
        let mut large = IndexMap::with_capacity(100_000);
        let mut reserved = IndexMap::new();
        for (i, op) in ops(42, 20_000, 500).into_iter().enumerate() {
            apply(&mut small, op, |k| k as u64);
            apply(&mut large, op, |k| k as u64);
            apply(&mut reserved, op, |k| k as u64);
            if i % 1000 == 0 {
                reserved.reserve(i);
            }
            assert_eq!(small.as_slice(), large.as_slice());
            assert_eq!(small.as_slice(), reserved.as_slice());
        }
    }

    #[test]
    fn save_and_load_keep_order_and_bytes() {
        let all = ops(1234, 30_000, 400);
        let (before, after) = all.split_at(10_000);
        let mut original = IndexMap::new();
        for &op in before {
            apply(&mut original, op, Weak);
        }
        assert!(original.len() > 10);

        let bytes = bincode::serialize(&original).unwrap();
        let mut restored: IndexMap<Weak, u64> = bincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.as_slice(), original.as_slice());
        assert_eq!(bincode::serialize(&restored).unwrap(), bytes);

        for &op in after {
            apply(&mut original, op, Weak);
            apply(&mut restored, op, Weak);
            assert_eq!(
                bincode::serialize(&restored).unwrap(),
                bincode::serialize(&original).unwrap()
            );
        }
    }

    #[test]
    fn load_rejects_duplicate_keys() {
        let bytes = bincode::serialize(&vec![(1u64, 2u64), (1, 3)]).unwrap();
        assert!(bincode::deserialize::<IndexMap<u64, u64>>(&bytes).is_err());
    }

    #[test]
    fn load_survives_a_corrupt_length() {
        let mut bytes = bincode::serialize(&IndexMap::from([(1u64, 2u64)])).unwrap();
        bytes[..8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(bincode::deserialize::<IndexMap<u64, u64>>(&bytes).is_err());
    }

    #[test]
    fn insert_replaces_in_place() {
        let mut map = IndexMap::new();
        assert_eq!(map.insert("a", 1), None);
        assert_eq!(map.insert("b", 2), None);
        assert_eq!(map.insert("a", 3), Some(1));
        assert_eq!(map.as_slice(), &[("a", 3), ("b", 2)]);
    }

    #[test]
    fn get_and_contains() {
        let mut map = IndexMap::from([(1u32, "one"), (2, "two")]);
        assert_eq!(map.get(&1), Some(&"one"));
        assert_eq!(map.get(&3), None);
        assert!(map.contains_key(&2));
        assert!(!map.contains_key(&3));
        assert_eq!(map[&2], "two");
        *map.get_mut(&1).unwrap() = "uno";
        assert_eq!(map[&1], "uno");
        assert_eq!(map.get_mut(&9), None);
    }

    #[test]
    #[should_panic(expected = "key not found")]
    fn index_panics_on_missing_key() {
        let map = IndexMap::from([(1u32, 1u32)]);
        let _ = map[&2];
    }

    #[test]
    fn empty_map_finds_nothing() {
        let mut map = IndexMap::<u64, u64>::new();
        assert!(map.is_empty());
        assert_eq!(map.get(&0), None);
        assert_eq!(map.swap_remove(&0), None);
        assert_eq!(map.iter().next(), None);
    }

    #[test]
    fn swap_remove_moves_the_last_entry() {
        let mut map = IndexMap::from([('a', 0), ('b', 1), ('c', 2), ('d', 3)]);
        assert_eq!(map.swap_remove(&'b'), Some(1));
        assert_eq!(map.as_slice(), &[('a', 0), ('d', 3), ('c', 2)]);
        assert_eq!(map[&'d'], 3);
        assert_eq!(map.swap_remove(&'b'), None);
        assert_eq!(map.swap_remove(&'c'), Some(2));
        assert_eq!(map.swap_remove(&'a'), Some(0));
        assert_eq!(map.as_slice(), &[('d', 3)]);
    }

    #[test]
    fn clear_then_reuse() {
        let mut map: IndexMap<u32, u32> = (0..50).map(|i| (i, i)).collect();
        map.clear();
        assert!(map.is_empty());
        assert_eq!(map.get(&3), None);
        map.insert(3, 4);
        assert_eq!(map.as_slice(), &[(3, 4)]);
    }

    #[test]
    fn grows_and_finds_everything() {
        let mut map = IndexMap::new();
        for i in 0..100_000u64 {
            map.insert(i * 7919, i);
        }
        assert_eq!(map.len(), 100_000);
        for i in 0..100_000u64 {
            assert_eq!(map.get(&(i * 7919)), Some(&i));
            assert_eq!(map.get(&(i * 7919 + 1)), None);
        }
        for i in (0..100_000u64).step_by(2) {
            assert_eq!(map.swap_remove(&(i * 7919)), Some(i));
        }
        for i in 0..100_000u64 {
            let expected = (i % 2 == 1).then_some(i);
            assert_eq!(map.get(&(i * 7919)).copied(), expected);
        }
    }

    #[test]
    fn iteration_follows_entry_order() {
        let mut map = IndexMap::from([(3u32, 30u32), (1, 10), (2, 20)]);
        for (key, value) in map.iter_mut() {
            *value += key;
        }
        let mut seen = Vec::new();
        for (key, value) in &map {
            seen.push((*key, *value));
        }
        assert_eq!(seen, [(3, 33), (1, 11), (2, 22)]);
        assert_eq!(map.iter().as_slice(), map.as_slice());
    }

    #[test]
    fn extend_and_collect_keep_first_position() {
        let mut map: IndexMap<u32, u32> = [(1, 1), (2, 2), (1, 3)].into_iter().collect();
        assert_eq!(map.as_slice(), &[(1, 3), (2, 2)]);
        map.extend([(4, 4), (2, 5)]);
        assert_eq!(map.as_slice(), &[(1, 3), (2, 5), (4, 4)]);
    }

    #[test]
    fn debug_shows_entries_in_order() {
        let map = IndexMap::from([(2u8, 'b'), (1, 'a')]);
        assert_eq!(format!("{map:?}"), "{2: 'b', 1: 'a'}");
    }
}
