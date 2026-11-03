use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Serialize, Deserialize, Debug)]
pub struct BitSet {
    pub words: Vec<u64>,
}

impl BitSet {
    #[inline]
    pub fn new() -> Self {
        Self { words: Vec::new() }
    }

    #[inline]
    fn extract(&self, index: usize) -> (usize, usize) {
        (index >> 6, index & 63)
    }

    #[inline]
    pub fn get(&self, index: usize) -> bool {
        let (bucket, bit) = self.extract(index);
        self.words
            .get(bucket)
            .map(|&word| (word & (1 << bit)) != 0)
            .unwrap_or(false)
    }

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

    #[inline]
    pub fn get_word(&self, index: usize) -> Option<u64> {
        self.words.get(index).copied()
    }

    #[inline]
    pub fn set_word(&mut self, index: usize, word: u64) {
        if index >= self.words.len() {
            self.words.resize(index + 1, 0u64);
        }
        self.words[index] = word;
    }

    #[inline]
    pub fn clear(&mut self) {
        self.words.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
