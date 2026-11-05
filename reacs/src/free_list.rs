use serde::{Deserialize, Serialize};

use crate::bit_set::BitSet;

#[derive(Default, Serialize, Deserialize)]
pub struct FreeList {
    pub bitset: BitSet,
    pub frees: u64,
}

impl FreeList {
    pub fn new() -> Self {
        Self {
            bitset: BitSet::new(),
            frees: 0,
        }
    }

    pub fn reserve(&mut self) -> u64 {
        let words = &mut self.bitset.words;

        for (w, word) in words.iter_mut().enumerate() {
            if *word < !0u64 {
                let o = word.trailing_ones();
                *word |= 1 << o;
                return w as u64 * 64 + o as u64;
            }
        }

        let next = words.len() * 64;
        self.bitset.set(next, true);
        return next as u64;
    }

    // pub fn reserve_many<const A: usize>(&mut self) -> [u64; A] {
    //     let words = &mut self.bitset.words;

    //     let mut amount = A;
    //     let mut reserved = [0u64; A];

    //     let mut w = 0;
    //     let word_count = words.len();

    //     while amount > 0 && w < word_count {
    //         let mut tb = words[w];
    //         let mut i = tb.trailing_ones();
    //         tb = tb.checked_shr(i).unwrap_or(0);

    //         while amount > 0 && i < 64 {
    //             let zeros = tb.trailing_zeros().min(64 - i);
    //             tb = tb.checked_shr(zeros).unwrap_or(0);

    //             let run_limit = zeros.min(amount as u32);
    //             let mask = (1u64.checked_shl(run_limit))
    //                 .unwrap_or(0)
    //                 .wrapping_sub(1);
    //             words[w] |= mask << i;

    //             for z in 0..run_limit {
    //                 reserved[A - amount] = (w as u64) * 64 + i as u64 + z as u64;
    //                 amount -= 1;
    //             }

    //             let ones = tb.trailing_ones().min(64 - i);
    //             tb = tb.checked_shr(ones).unwrap_or(0);
    //             i += zeros + ones;
    //         }

    //         w += 1;
    //     }

    //     if amount > 0 {
    //         let extra_words = (amount + 63) / 64;
    //         for extra in 0..extra_words {
    //             let word_index = w + extra;
    //             let bits_this_word = amount.min(64);
    //             let mask = (1u64.checked_shl(bits_this_word as u32))
    //                 .unwrap_or(0)
    //                 .wrapping_sub(1);
    //             self.bitset.set_word(word_index, mask);

    //             for z in 0..bits_this_word {
    //                 reserved[A - amount] = (word_index as u64) * 64 + z as u64;
    //                 amount -= 1;
    //             }
    //         }
    //     }

    //     reserved
    // }

    pub fn free(&mut self, index: usize) {
        self.bitset.set(index, false);
        self.frees += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserves_sequentially_across_words() {
        let mut list = FreeList::new();
        for expected in 0..200 {
            assert_eq!(list.reserve(), expected);
        }
        assert_eq!(list.bitset.words.len(), 4);
    }

    #[test]
    fn reserve_and_free_update_the_index_bit() {
        let mut list = FreeList::new();
        let index = list.reserve() as usize;
        assert!(list.bitset.get(index));
        assert!(!list.bitset.get(index + 1));

        list.free(index);
        assert!(!list.bitset.get(index));
    }

    #[test]
    fn lowest_freed_index_is_reused_first() {
        let mut list = FreeList::new();
        for _ in 0..130 {
            list.reserve();
        }
        list.free(100);
        list.free(10);
        list.free(3);
        assert_eq!(list.reserve(), 3);
        assert_eq!(list.reserve(), 10);
        assert_eq!(list.reserve(), 100);
        assert_eq!(list.reserve(), 130);
    }

    #[test]
    fn full_words_are_skipped() {
        let mut list = FreeList::new();
        for _ in 0..130 {
            list.reserve();
        }
        list.free(70);
        assert_eq!(list.reserve(), 70);
        assert_eq!(list.reserve(), 130);
    }

    #[test]
    fn first_bit_of_a_word_is_reused_when_it_is_the_only_free_one() {
        let mut list = FreeList::new();
        for _ in 0..64 {
            list.reserve();
        }
        list.free(0);
        assert_eq!(list.reserve(), 0);
        assert_eq!(list.reserve(), 64);
    }

    #[test]
    fn last_bit_of_a_word_is_reserved_before_a_new_word() {
        let mut list = FreeList::new();
        for _ in 0..64 {
            list.reserve();
        }
        assert_eq!(list.bitset.words.len(), 1);
        list.free(63);
        assert_eq!(list.reserve(), 63);
        assert_eq!(list.bitset.words.len(), 1);
        assert_eq!(list.reserve(), 64);
        assert_eq!(list.bitset.words.len(), 2);
    }

    #[test]
    fn freeing_an_unreserved_index_does_nothing() {
        let mut list = FreeList::new();
        list.free(1_000);
        assert!(list.bitset.words.is_empty());

        assert_eq!(list.reserve(), 0);
        list.free(5);
        assert_eq!(list.reserve(), 1);
    }

    #[test]
    fn serialization_round_trip_keeps_reservations() {
        let mut list = FreeList::new();
        for _ in 0..70 {
            list.reserve();
        }
        list.free(5);

        let bytes = bincode::serialize(&list).unwrap();
        let mut restored: FreeList = bincode::deserialize(&bytes).unwrap();

        assert_eq!(restored.reserve(), 5);
        assert_eq!(restored.reserve(), 70);
    }
}
