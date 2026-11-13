use serde::{Deserialize, Serialize};

use crate::bit_set::BitSet;

#[derive(Default, Serialize, Deserialize)]
pub struct FreeList {
    pub bitset: BitSet,
    pub frees: u64,
    #[serde(skip)]
    full: Vec<u64>,
    #[serde(skip)]
    first_open: usize,
    #[serde(skip)]
    built: bool,
}

impl FreeList {
    #[cold]
    fn rebuild(&mut self) {
        self.full = self
            .bitset
            .words
            .chunks(64)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0u64, |acc, (b, &w)| acc | (((w == !0u64) as u64) << b))
            })
            .collect();
        self.first_open = 0;
        self.built = true;
        #[cfg(any(test, feature = "invariants"))]
        self.check_invariants();
    }

    #[cfg(any(test, feature = "invariants"))]
    pub(crate) fn check_invariants(&self) {
        if !self.built {
            return;
        }
        let words = &self.bitset.words;
        assert_eq!(self.full.len(), words.len().div_ceil(64), "summary length");
        assert!(
            self.first_open <= self.full.len(),
            "first_open past the summary"
        );
        for (w, &word) in words.iter().enumerate() {
            let marked = self.full[w >> 6] >> (w & 63) & 1 == 1;
            assert_eq!(marked, word == !0u64, "summary bit of word {w}");
        }
        for s in 0..self.first_open {
            assert_eq!(
                self.full[s], !0u64,
                "free index below first_open in summary word {s}"
            );
        }
    }

    pub fn reserve(&mut self) -> u64 {
        if !self.built {
            self.rebuild();
        }
        let words = &mut self.bitset.words;
        let full = &mut self.full;

        // `s` also numbers the summary word, to find the bitset word `w`; this is measured spawn
        // code, so it keeps the indexed loop clippy would turn into an iterator.
        #[allow(clippy::needless_range_loop)]
        for s in self.first_open..full.len() {
            let summary = full[s];
            if summary != !0u64 {
                let b = summary.trailing_ones() as usize;
                let w = s * 64 + b;
                if w >= words.len() {
                    break;
                }
                let word = &mut words[w];
                let o = word.trailing_ones();
                *word |= 1 << o;
                if *word == !0u64 {
                    full[s] |= 1 << b;
                }
                debug_assert_eq!(full[s] >> b & 1 == 1, words[w] == !0u64);
                self.first_open = s;
                return w as u64 * 64 + o as u64;
            }
        }

        let w = words.len();
        words.push(1);
        if w >> 6 >= full.len() {
            full.push(0);
        }
        self.first_open = w >> 6;
        w as u64 * 64
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
        let w = index >> 6;
        if let Some(word) = self.bitset.words.get_mut(w) {
            *word &= !(1 << (index & 63));
            if self.built {
                self.full[w >> 6] &= !(1 << (w & 63));
                self.first_open = self.first_open.min(w >> 6);
                debug_assert!(self.first_open <= w >> 6, "freed word {w} below first_open");
            }
        }
        self.frees += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserves_sequentially_across_words() {
        let mut list = FreeList::default();
        for expected in 0..200 {
            assert_eq!(list.reserve(), expected);
        }
        assert_eq!(list.bitset.words.len(), 4);
    }

    #[test]
    fn reserve_and_free_update_the_index_bit() {
        let mut list = FreeList::default();
        let index = list.reserve() as usize;
        assert!(list.bitset.get(index));
        assert!(!list.bitset.get(index + 1));

        list.free(index);
        assert!(!list.bitset.get(index));
    }

    #[test]
    fn lowest_freed_index_is_reused_first() {
        let mut list = FreeList::default();
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
        let mut list = FreeList::default();
        for _ in 0..130 {
            list.reserve();
        }
        list.free(70);
        assert_eq!(list.reserve(), 70);
        assert_eq!(list.reserve(), 130);
    }

    #[test]
    fn first_bit_of_a_word_is_reused_when_it_is_the_only_free_one() {
        let mut list = FreeList::default();
        for _ in 0..64 {
            list.reserve();
        }
        list.free(0);
        assert_eq!(list.reserve(), 0);
        assert_eq!(list.reserve(), 64);
    }

    #[test]
    fn last_bit_of_a_word_is_reserved_before_a_new_word() {
        let mut list = FreeList::default();
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
        let mut list = FreeList::default();
        list.free(1_000);
        assert!(list.bitset.words.is_empty());

        assert_eq!(list.reserve(), 0);
        list.free(5);
        assert_eq!(list.reserve(), 1);
    }

    #[test]
    fn serialization_round_trip_keeps_reservations() {
        let mut list = FreeList::default();
        for _ in 0..70 {
            list.reserve();
        }
        list.free(5);

        let bytes = bincode::serialize(&list).unwrap();
        let mut restored: FreeList = bincode::deserialize(&bytes).unwrap();

        assert_eq!(restored.reserve(), 5);
        assert_eq!(restored.reserve(), 70);
    }

    #[test]
    fn freeing_below_the_open_word_is_reused_before_the_end() {
        let mut list = FreeList::default();
        for _ in 0..300 {
            list.reserve();
        }
        list.free(200);
        assert_eq!(list.reserve(), 200);
        list.free(7);
        list.free(130);
        assert_eq!(list.reserve(), 7);
        assert_eq!(list.reserve(), 130);
        assert_eq!(list.reserve(), 300);
    }

    #[test]
    fn restored_list_reserves_like_the_original() {
        let mut list = FreeList::default();
        for _ in 0..1_000 {
            list.reserve();
        }
        for index in [3, 640, 641, 999] {
            list.free(index);
        }
        list.reserve();

        let bytes = bincode::serialize(&list).unwrap();
        let mut restored: FreeList = bincode::deserialize(&bytes).unwrap();
        for _ in 0..5 {
            assert_eq!(restored.reserve(), list.reserve());
        }
    }

    #[test]
    fn always_reserves_the_lowest_free_index() {
        let mut list = FreeList::default();
        let mut used = Vec::<bool>::new();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for _ in 0..20_000 {
            let roll = next();
            if roll % 5 < 3 || used.is_empty() {
                let expected = used.iter().position(|&u| !u).unwrap_or(used.len());
                if expected == used.len() {
                    used.push(false);
                }
                used[expected] = true;
                assert_eq!(list.reserve(), expected as u64);
            } else {
                let index = (next() % used.len() as u64) as usize;
                used[index] = false;
                list.free(index);
            }
        }
    }
}
