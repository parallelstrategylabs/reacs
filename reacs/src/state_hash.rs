use std::fmt;

use serde::{
    Serialize, Serializer,
    ser::{
        self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
        SerializeTupleStruct, SerializeTupleVariant,
    },
};

const FIRST: u64 = 0x9E37_79B9_7F4A_7C15;
const BLOCK_KEY: u64 = 0x8EBC_6AF0_9C88_C6E3;

#[inline(always)]
fn mix(a: u64, b: u64) -> u64 {
    let product = (a as u128).wrapping_mul(b as u128);
    product as u64 ^ (product >> 64) as u64
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = StateHasher::new();
    hasher.write_bytes(bytes);
    hasher.finish()
}

pub(crate) fn hash_value<T: Serialize>(value: &T) -> u64 {
    let mut hasher = StateHasher::new();
    value
        .serialize(&mut hasher)
        .expect("failed to hash a resource");
    hasher.finish()
}

#[derive(Debug)]
pub(crate) struct HashError;

impl fmt::Display for HashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to hash a value")
    }
}

impl std::error::Error for HashError {}

impl ser::Error for HashError {
    fn custom<T: fmt::Display>(_message: T) -> Self {
        HashError
    }
}

pub(crate) struct StateHasher {
    state: u64,
    buffer: [u64; 2],
    used: usize,
    pending: u64,
    filled: u32,
}

impl StateHasher {
    #[inline(always)]
    fn new() -> Self {
        Self {
            state: FIRST,
            buffer: [0; 2],
            used: 0,
            pending: 0,
            filled: 0,
        }
    }

    #[inline(always)]
    fn flush(&mut self) {
        self.buffer[self.used & 1] = self.pending;
        self.used += 1;
        self.pending = 0;
        self.filled = 0;
        if self.used == 2 {
            self.used = 0;
            self.state = mix(self.buffer[0] ^ BLOCK_KEY, self.buffer[1] ^ self.state);
        }
    }

    #[inline(always)]
    fn write(&mut self, value: u64, bits: u32) {
        if self.filled + bits > 64 {
            self.flush();
        }
        self.pending |= value << self.filled;
        self.filled += bits;
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        self.write(bytes.len() as u64, 64);
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            self.write(u64::from_le_bytes(chunk.try_into().unwrap()), 64);
        }
        for byte in chunks.remainder() {
            self.write(*byte as u64, 8);
        }
    }

    #[inline(always)]
    fn finish(mut self) -> u64 {
        self.flush();
        let odd = self.used == 1;
        if odd {
            self.flush();
        }
        mix(self.state ^ odd as u64, FIRST)
    }
}

macro_rules! write_as {
    ($($method:ident: $ty:ty as $unsigned:ty),* $(,)?) => {
        $(
            #[inline(always)]
            fn $method(self, value: $ty) -> Result<(), HashError> {
                self.write(value as $unsigned as u64, <$unsigned>::BITS);
                Ok(())
            }
        )*
    };
}

impl Serializer for &mut StateHasher {
    type Ok = ();
    type Error = HashError;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    write_as!(
        serialize_i8: i8 as u8,
        serialize_i16: i16 as u16,
        serialize_i32: i32 as u32,
        serialize_i64: i64 as u64,
        serialize_u8: u8 as u8,
        serialize_u16: u16 as u16,
        serialize_u32: u32 as u32,
        serialize_u64: u64 as u64,
    );

    #[inline(always)]
    fn serialize_bool(self, value: bool) -> Result<(), HashError> {
        self.write(value as u64, 8);
        Ok(())
    }

    #[inline(always)]
    fn serialize_i128(self, value: i128) -> Result<(), HashError> {
        self.serialize_u128(value as u128)
    }

    #[inline(always)]
    fn serialize_u128(self, value: u128) -> Result<(), HashError> {
        self.write(value as u64, 64);
        self.write((value >> 64) as u64, 64);
        Ok(())
    }

    #[inline(always)]
    fn serialize_f32(self, value: f32) -> Result<(), HashError> {
        self.write(value.to_bits() as u64, 32);
        Ok(())
    }

    #[inline(always)]
    fn serialize_f64(self, value: f64) -> Result<(), HashError> {
        self.write(value.to_bits(), 64);
        Ok(())
    }

    #[inline(always)]
    fn serialize_char(self, value: char) -> Result<(), HashError> {
        self.write(value as u64, 32);
        Ok(())
    }

    fn serialize_str(self, value: &str) -> Result<(), HashError> {
        self.write_bytes(value.as_bytes());
        Ok(())
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<(), HashError> {
        self.write_bytes(value);
        Ok(())
    }

    #[inline(always)]
    fn serialize_none(self) -> Result<(), HashError> {
        self.write(0, 8);
        Ok(())
    }

    #[inline(always)]
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), HashError> {
        self.write(1, 8);
        value.serialize(self)
    }

    #[inline(always)]
    fn serialize_unit(self) -> Result<(), HashError> {
        Ok(())
    }

    #[inline(always)]
    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), HashError> {
        Ok(())
    }

    #[inline(always)]
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        index: u32,
        _variant: &'static str,
    ) -> Result<(), HashError> {
        self.write(index as u64, 32);
        Ok(())
    }

    #[inline(always)]
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), HashError> {
        value.serialize(self)
    }

    #[inline(always)]
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<(), HashError> {
        self.write(index as u64, 32);
        value.serialize(self)
    }

    #[inline(always)]
    fn serialize_seq(self, len: Option<usize>) -> Result<Self, HashError> {
        self.write(len.map_or(u64::MAX, |len| len as u64), 64);
        Ok(self)
    }

    #[inline(always)]
    fn serialize_tuple(self, _len: usize) -> Result<Self, HashError> {
        Ok(self)
    }

    #[inline(always)]
    fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Self, HashError> {
        Ok(self)
    }

    #[inline(always)]
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self, HashError> {
        self.write(index as u64, 32);
        Ok(self)
    }

    #[inline(always)]
    fn serialize_map(self, len: Option<usize>) -> Result<Self, HashError> {
        self.write(len.map_or(u64::MAX, |len| len as u64), 64);
        Ok(self)
    }

    #[inline(always)]
    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self, HashError> {
        Ok(self)
    }

    #[inline(always)]
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self, HashError> {
        self.write(index as u64, 32);
        Ok(self)
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

macro_rules! compound {
    ($($name:ident :: $method:ident),* $(,)?) => {
        $(
            impl $name for &mut StateHasher {
                type Ok = ();
                type Error = HashError;

                #[inline(always)]
                fn $method<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), HashError> {
                    value.serialize(&mut **self)
                }

                #[inline(always)]
                fn end(self) -> Result<(), HashError> {
                    Ok(())
                }
            }
        )*
    };
}

compound!(
    SerializeSeq::serialize_element,
    SerializeTuple::serialize_element,
    SerializeTupleStruct::serialize_field,
    SerializeTupleVariant::serialize_field,
);

impl SerializeMap for &mut StateHasher {
    type Ok = ();
    type Error = HashError;

    #[inline(always)]
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), HashError> {
        key.serialize(&mut **self)
    }

    #[inline(always)]
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), HashError> {
        value.serialize(&mut **self)
    }

    #[inline(always)]
    fn end(self) -> Result<(), HashError> {
        Ok(())
    }
}

macro_rules! named_fields {
    ($($name:ident),* $(,)?) => {
        $(
            impl $name for &mut StateHasher {
                type Ok = ();
                type Error = HashError;

                #[inline(always)]
                fn serialize_field<T: ?Sized + Serialize>(
                    &mut self,
                    _key: &'static str,
                    value: &T,
                ) -> Result<(), HashError> {
                    value.serialize(&mut **self)
                }

                #[inline(always)]
                fn end(self) -> Result<(), HashError> {
                    Ok(())
                }
            }
        )*
    };
}

named_fields!(SerializeStruct, SerializeStructVariant);

#[cfg(test)]
mod tests {
    use super::{FIRST, hash_bytes, hash_value};
    use serde::Serialize;

    #[derive(Serialize)]
    #[allow(dead_code)]
    struct Unit {
        health: i32,
        speed: f32,
        #[serde(skip)]
        cache: u64,
    }

    #[derive(Serialize)]
    enum Mode {
        Idle,
        Moving(u32),
    }

    #[test]
    fn a_changed_field_changes_the_hash() {
        let hash = |health, speed| {
            hash_value(&Unit {
                health,
                speed,
                cache: 0,
            })
        };
        assert_eq!(hash(1, 2.0), hash(1, 2.0));
        assert_ne!(hash(1, 2.0), hash(2, 2.0));
        assert_ne!(hash(1, 2.0), hash(1, 2.5));
    }

    #[test]
    fn a_skipped_field_is_not_hashed() {
        let hash = |cache| {
            hash_value(&Unit {
                health: 1,
                speed: 2.0,
                cache,
            })
        };
        assert_eq!(hash(1), hash(99));
    }

    #[test]
    fn negative_zero_and_zero_are_different_states() {
        assert_ne!(hash_value(&0.0f32), hash_value(&-0.0f32));
    }

    #[test]
    fn sequences_of_different_lengths_differ() {
        assert_ne!(hash_value(&vec![0u8; 3]), hash_value(&vec![0u8; 4]));
        assert_ne!(hash_value(&Vec::<u64>::new()), hash_value(&vec![0u64]));
    }

    #[test]
    fn values_past_the_first_word_still_count() {
        let values = |last: u8| {
            let mut bytes = vec![7u8; 40];
            bytes.push(last);
            hash_value(&bytes)
        };
        assert_ne!(values(1), values(2));
    }

    #[test]
    fn an_all_zero_value_still_depends_on_its_size() {
        assert_ne!(hash_value(&[0u64; 2]), hash_value(&[0u64; 3]));
    }

    #[test]
    fn an_all_zero_value_of_any_size_has_its_own_hash() {
        let hashes = [
            hash_value(&[0u64; 1]),
            hash_value(&[0u64; 2]),
            hash_value(&[0u64; 3]),
            hash_value(&[0u64; 4]),
            hash_value(&[0u64; 5]),
        ];
        for (i, first) in hashes.iter().enumerate() {
            for second in &hashes[i + 1..] {
                assert_ne!(first, second);
            }
        }
    }

    #[test]
    fn a_common_seed_constant_does_not_erase_what_came_before() {
        assert_ne!(
            hash_value(&[1u64, 2, FIRST, 7]),
            hash_value(&[9u64, 2, FIRST, 7])
        );
    }

    #[test]
    fn both_words_of_a_block_count() {
        assert_ne!(hash_value(&[1u64, 2]), hash_value(&[3u64, 2]));
        assert_ne!(hash_value(&[1u64, 2]), hash_value(&[1u64, 4]));
    }

    #[test]
    fn enum_variants_and_options_differ() {
        assert_ne!(hash_value(&Mode::Idle), hash_value(&Mode::Moving(0)));
        assert_ne!(hash_value(&None::<u32>), hash_value(&Some(0u32)));
    }

    #[test]
    fn strings_and_bytes_are_hashed_by_content() {
        assert_eq!(hash_value(&"same"), hash_value(&"same"));
        assert_ne!(hash_value(&"same"), hash_value(&"sane"));
        assert_ne!(hash_bytes(b"12345678"), hash_bytes(b"12345679"));
    }
}
