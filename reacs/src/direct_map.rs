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
        self.has(key).then(|| &self.values[key as usize])
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
