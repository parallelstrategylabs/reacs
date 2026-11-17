//! The binary format of saves and recordings: postcard, kept behind one module so every call site
//! encodes the same way.

use serde::{Serialize, de::DeserializeOwned};
use std::io::Read;

pub(crate) type Error = postcard::Error;

/// Encodes `value`.
pub(crate) fn to_vec<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, Error> {
    postcard::to_allocvec(value)
}

/// Decodes a value that must fill `bytes` exactly.
pub(crate) fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    let (value, rest) = postcard::take_from_bytes(bytes)?;
    if rest.is_empty() {
        Ok(value)
    } else {
        Err(Error::DeserializeBadEncoding)
    }
}

/// Decodes a value from the front of `bytes`, returning it and the bytes after it.
pub(crate) fn take<T: DeserializeOwned>(bytes: &[u8]) -> Result<(T, &[u8]), Error> {
    postcard::take_from_bytes(bytes)
}

/// Reads exactly one value's bytes from `reader`.
///
/// The scratch buffer only holds borrowed data while decoding; recordings decode numbers and byte
/// vectors, which postcard reads a byte at a time, so it never needs to be larger.
pub(crate) fn read<T: DeserializeOwned, R: Read>(reader: R) -> Result<T, Error> {
    let mut scratch = [0u8; 1024];
    postcard::from_io((reader, &mut scratch[..])).map(|(value, _)| value)
}
