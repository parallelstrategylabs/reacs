//! A growable set of indexes stored as bits.

use serde::{Deserialize, Serialize};

/// A growable set of `usize` indexes, one bit per index.
///
/// Index `i` is bit `i % 64` of word `i / 64`. Setting an index past the end grows the set;
/// reading or clearing one past the end treats it as unset. Memory follows the largest index set,
/// not the number of indexes, so it suits dense indexes like entity indexes.
///
/// # Examples
///
/// ```
/// use reacs::bit_set::BitSet;
///
/// let mut set = BitSet::new();
/// set.set(3, true);
/// set.set(70, true);
///
/// assert!(set.get(70));
/// assert!(!set.get(4));
/// assert_eq!(set.next_set(4, 100), 70);
/// assert_eq!(set.next_set(71, 100), 100);
/// ```
#[derive(Default, Clone, Serialize, Deserialize, Debug)]
pub struct BitSet {
    /// The bits, 64 indexes per word: index `i` is bit `i % 64` of `words[i / 64]`.
    pub words: Vec<u64>,
}

impl BitSet {
    /// Creates an empty set.
    #[inline]
    pub fn new() -> Self {
        Self { words: Vec::new() }
    }

    #[inline]
    fn extract(&self, index: usize) -> (usize, usize) {
        (index >> 6, index & 63)
    }

    /// Returns `true` if `index` is in the set.
    #[inline]
    pub fn get(&self, index: usize) -> bool {
        let (bucket, bit) = self.extract(index);
        self.words
            .get(bucket)
            .map(|&word| (word & (1 << bit)) != 0)
            .unwrap_or(false)
    }

    /// Adds `index` to the set if `state` is `true`, or removes it if `false`.
    ///
    /// Adding an index past the end grows the set; removing one does nothing.
    #[inline]
    pub fn set(&mut self, index: usize, state: bool) {
        let (bucket, bit) = self.extract(index);
        if state {
            if bucket >= self.words.len() {
                self.words.resize(bucket + 1, 0u64);
            }
            self.words[bucket] |= 1 << bit;
        } else {
            if let Some(word) = self.words.get_mut(bucket) {
                *word &= !(1 << bit);
            }
        }
    }

    /// Returns the first index in the set within `from..limit`, or `limit` if there's none.
    ///
    /// It skips 64 indexes at a time over empty words.
    #[inline]
    pub fn next_set(&self, from: usize, limit: usize) -> usize {
        let mut index = from;
        while index < limit {
            let (bucket, bit) = self.extract(index);
            let Some(&word) = self.words.get(bucket) else {
                return limit;
            };
            let rest = word >> bit;
            if rest != 0 {
                return (index + rest.trailing_zeros() as usize).min(limit);
            }
            index = (bucket + 1) << 6;
        }
        limit
    }

    /// Returns word `index`, the bits of indexes `index * 64` to `index * 64 + 63`, or `None` past
    /// the end.
    #[inline]
    pub fn get_word(&self, index: usize) -> Option<u64> {
        self.words.get(index).copied()
    }

    /// Replaces word `index` with `word`, growing the set if needed.
    #[inline]
    pub fn set_word(&mut self, index: usize, word: u64) {
        if index >= self.words.len() {
            self.words.resize(index + 1, 0u64);
        }
        self.words[index] = word;
    }

    /// Removes every index, keeping the memory.
    #[inline]
    pub fn clear(&mut self) {
        self.words.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_set_finds_bits_across_words_and_respects_limit() {
        let mut set = BitSet::new();
        for index in [3, 63, 64, 200] {
            set.set(index, true);
        }

        assert_eq!(set.next_set(0, 300), 3);
        assert_eq!(set.next_set(3, 300), 3);
        assert_eq!(set.next_set(4, 300), 63);
        assert_eq!(set.next_set(64, 300), 64);
        assert_eq!(set.next_set(65, 300), 200);
        assert_eq!(set.next_set(201, 300), 300);
        assert_eq!(set.next_set(65, 100), 100);
        assert_eq!(set.next_set(10, 10), 10);
        assert_eq!(BitSet::new().next_set(5, 50), 50);
    }

    #[test]
    fn new_set_has_no_words_and_no_bits() {
        let set = BitSet::new();
        assert!(set.words.is_empty());
        assert!(!set.get(0));
        assert!(!set.get(1_000_000));
    }

    #[test]
    fn set_and_get_across_word_boundaries() {
        let mut set = BitSet::new();
        let indices = [0, 1, 63, 64, 65, 127, 128];
        for index in indices {
            assert!(!set.get(index));
            set.set(index, true);
            assert!(set.get(index));
        }
        for index in 0..200 {
            assert_eq!(set.get(index), indices.contains(&index));
        }
    }

    #[test]
    fn setting_a_bit_grows_only_to_its_word() {
        let mut set = BitSet::new();
        set.set(63, true);
        assert_eq!(set.words.len(), 1);
        set.set(64, true);
        assert_eq!(set.words.len(), 2);
        set.set(640, true);
        assert_eq!(set.words.len(), 11);
    }

    #[test]
    fn unsetting_clears_only_that_bit() {
        let mut set = BitSet::new();
        set.set(3, true);
        set.set(4, true);
        set.set(3, false);
        assert!(!set.get(3));
        assert!(set.get(4));
    }

    #[test]
    fn unsetting_past_the_end_does_not_grow() {
        let mut set = BitSet::new();
        set.set(1_000, false);
        assert!(set.words.is_empty());
    }

    #[test]
    fn get_word_returns_none_past_the_end() {
        let mut set = BitSet::new();
        assert_eq!(set.get_word(0), None);
        set.set(65, true);
        assert_eq!(set.get_word(0), Some(0));
        assert_eq!(set.get_word(1), Some(0b10));
        assert_eq!(set.get_word(2), None);
    }

    #[test]
    fn set_word_grows_with_zeroed_gap_and_overwrites() {
        let mut set = BitSet::new();
        set.set_word(2, 0b101);
        assert_eq!(set.words, vec![0, 0, 0b101]);
        assert!(set.get(128));
        assert!(!set.get(129));
        assert!(set.get(130));

        set.set_word(2, 0b10);
        assert_eq!(set.words, vec![0, 0, 0b10]);
    }

    #[test]
    fn clear_unsets_every_bit_and_keeps_the_words() {
        let mut set = BitSet::new();
        set.set(1, true);
        set.set(100, true);
        set.clear();
        assert!(!set.get(1));
        assert!(!set.get(100));
        assert_eq!(set.words, vec![0, 0]);
    }
}
