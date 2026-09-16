#[derive(Default, Clone, Debug)]
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
    pub fn resize(&mut self, new_len: usize, value: bool) {
        if new_len == 0 {
            return;
        }

        let needed_buckets = (new_len + 63) >> 6;
        let fill_value = if value { u64::MAX } else { 0 };
        self.words.resize(needed_buckets, fill_value);
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
