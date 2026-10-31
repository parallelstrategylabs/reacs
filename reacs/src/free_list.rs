use serde::{Deserialize, Serialize};

use crate::bit_set::BitSet;

#[derive(Default, Serialize, Deserialize)]
pub struct FreeList {
    pub bitset: BitSet,
}

impl FreeList {
    pub fn new() -> Self {
        Self {
            bitset: BitSet::new(),
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
    }
}
