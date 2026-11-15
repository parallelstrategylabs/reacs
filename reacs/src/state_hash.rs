//! The hash reacs uses for world state, the same on every platform and build.

use std::fmt::Error;

use serde::{
    Serialize, Serializer,
    ser::{
        SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
        SerializeTupleStruct, SerializeTupleVariant,
    },
};

const FIRST: u64 = 0x9E37_79B9_7F4A_7C15;
const BLOCK_KEY: u64 = 0x8EBC_6AF0_9C88_C6E3;

type Done = Result<(), Error>;
type Name = &'static str;

#[inline(always)]
fn mix(a: u64, b: u64) -> u64 {
    let product = (a as u128).wrapping_mul(b as u128);
    product as u64 ^ (product >> 64) as u64
}

/// Hashes `bytes` with the hash reacs uses for world state.
///
/// The same bytes give the same hash on every platform and build.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = StateHasher::new();
    hasher.write_bytes(bytes);
    hasher.finish()
}

/// Hashes `value` the way [`World::resource_hashes`](crate::world::World::resource_hashes) hashes a
/// resource.
///
/// It feeds the value's serde serialization straight into the hash, without encoding it first, so
/// it skips what saves skip (`#[serde(skip)]` fields) and gives the same result on every platform
/// and build, floats included.
///
/// # Panics
///
/// Panics if the value's `Serialize` implementation fails.
pub fn hash_value<T: Serialize>(value: &T) -> u64 {
    let mut hasher = StateHasher::new();
    value
        .serialize(&mut hasher)
        .expect("failed to hash a resource");
    hasher.finish()
}

struct StateHasher {
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
            self.state ^= mix(self.buffer[0] ^ BLOCK_KEY, self.buffer[1] ^ self.state);
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
        let (chunks, remainder) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.write(u64::from_le_bytes(*chunk), 64);
        }
        for byte in remainder {
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

macro_rules! write_value {
    ($($method:ident($value:ident: $ty:ty) => ($word:expr, $bits:expr);)*) => {$(
        #[inline(always)]
        fn $method(self, $value: $ty) -> Done {
            self.write($word, $bits);
            Ok(())
        }
    )*};
}

impl Serializer for &mut StateHasher {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    write_value! {
        serialize_bool(v: bool) => (v as u64, 8);
        serialize_i8(v: i8) => (v as u8 as u64, 8);
        serialize_i16(v: i16) => (v as u16 as u64, 16);
        serialize_i32(v: i32) => (v as u32 as u64, 32);
        serialize_i64(v: i64) => (v as u64, 64);
        serialize_u8(v: u8) => (v as u64, 8);
        serialize_u16(v: u16) => (v as u64, 16);
        serialize_u32(v: u32) => (v as u64, 32);
        serialize_u64(v: u64) => (v, 64);
        serialize_f32(v: f32) => (v.to_bits() as u64, 32);
        serialize_f64(v: f64) => (v.to_bits(), 64);
        serialize_char(v: char) => (v as u64, 32);
    }

    #[inline(always)]
    fn serialize_unit(self) -> Done {
        Ok(())
    }

    #[inline(always)]
    fn serialize_unit_struct(self, _: Name) -> Done {
        Ok(())
    }

    #[inline(always)]
    fn serialize_tuple(self, _: usize) -> Result<Self, Error> {
        Ok(self)
    }

    #[inline(always)]
    fn serialize_tuple_struct(self, _: Name, _: usize) -> Result<Self, Error> {
        Ok(self)
    }

    #[inline(always)]
    fn serialize_struct(self, _: Name, _: usize) -> Result<Self, Error> {
        Ok(self)
    }

    #[inline(always)]
    fn serialize_i128(self, value: i128) -> Done {
        self.serialize_u128(value as u128)
    }

    #[inline(always)]
    fn serialize_u128(self, value: u128) -> Done {
        self.write(value as u64, 64);
        self.serialize_u64((value >> 64) as u64)
    }

    fn serialize_str(self, value: &str) -> Done {
        self.serialize_bytes(value.as_bytes())
    }

    fn serialize_bytes(self, value: &[u8]) -> Done {
        self.write_bytes(value);
        Ok(())
    }

    #[inline(always)]
    fn serialize_none(self) -> Done {
        self.serialize_u8(0)
    }

    #[inline(always)]
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Done {
        self.write(1, 8);
        value.serialize(self)
    }

    #[inline(always)]
    fn serialize_unit_variant(self, _: Name, index: u32, _: Name) -> Done {
        self.serialize_u32(index)
    }

    #[inline(always)]
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: Name, value: &T) -> Done {
        value.serialize(self)
    }

    #[inline(always)]
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: Name,
        index: u32,
        _: Name,
        value: &T,
    ) -> Done {
        self.write(index as u64, 32);
        value.serialize(self)
    }

    #[inline(always)]
    fn serialize_seq(self, len: Option<usize>) -> Result<Self, Error> {
        self.write(len.map_or(u64::MAX, |len| len as u64), 64);
        Ok(self)
    }

    #[inline(always)]
    fn serialize_map(self, len: Option<usize>) -> Result<Self, Error> {
        self.serialize_seq(len)
    }

    #[inline(always)]
    fn serialize_tuple_variant(
        self,
        _: Name,
        index: u32,
        _: Name,
        _: usize,
    ) -> Result<Self, Error> {
        self.write(index as u64, 32);
        Ok(self)
    }

    #[inline(always)]
    fn serialize_struct_variant(
        self,
        name: Name,
        index: u32,
        variant: Name,
        len: usize,
    ) -> Result<Self, Error> {
        self.serialize_tuple_variant(name, index, variant, len)
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

macro_rules! compound {
    ($($name:ident: $($method:ident($($key:ty)?))+;)*) => {$(
        impl $name for &mut StateHasher {
            type Ok = ();
            type Error = Error;

            $(
                #[inline(always)]
                fn $method<T: ?Sized + Serialize>(&mut self, $(_: $key,)? value: &T) -> Done {
                    value.serialize(&mut **self)
                }
            )+

            #[inline(always)]
            fn end(self) -> Done {
                Ok(())
            }
        }
    )*};
}

compound! {
    SerializeSeq: serialize_element();
    SerializeTuple: serialize_element();
    SerializeTupleStruct: serialize_field();
    SerializeTupleVariant: serialize_field();
    SerializeMap: serialize_key() serialize_value();
    SerializeStruct: serialize_field(Name);
    SerializeStructVariant: serialize_field(Name);
}

#[cfg(test)]
mod tests {
    use super::{BLOCK_KEY, FIRST, hash_bytes, hash_value};
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
    fn a_word_equal_to_the_key_does_not_erase_what_came_before() {
        assert_ne!(
            hash_value(&[1u64, 2, BLOCK_KEY, 7]),
            hash_value(&[9u64, 2, BLOCK_KEY, 7])
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
