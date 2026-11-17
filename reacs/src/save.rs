//! How saved values turn into bytes and back.
//!
//! Every type stored in a world implements [`Save`]. Every value has one canonical encoding: its
//! fields in declaration order, numbers little-endian, `usize` and `isize` as 64-bit, lengths as
//! `u64`. The same state gives the same bytes on every build and platform.
//!
//! Each type writes that encoding by one of these rules, picked from its fields:
//!
//! 1. **Plain**: the value's memory already is its encoding: every field plain, laid out in
//!    declaration order with no padding, on a little-endian target. A value, or a whole `Vec` of
//!    them, is saved and loaded with one memory copy.
//! 2. **Plain, checked**: plain, but some fields only allow some bit patterns: `bool`, `char` and
//!    enums without data. Loading copies the memory after one pass that checks those fields, so a
//!    corrupted save panics instead of making an invalid value.
//! 3. **Field by field**: everything else: types with padding or reordered fields, enums with data,
//!    `Option`, `String`, `Vec`, `IndexMap`. Each field is written by its own rule.
//!
//! Rules 1 and 2 are only faster ways to write rule 3's bytes, so a type saves the same bytes
//! whichever rule a platform picks for it.
//!
//! Prefer fixed-size integers to `usize` and `isize` in saved types: they're always saved as 64-bit,
//! so a type holding one is only plain on 64-bit targets, and a value too big for a 32-bit `usize`
//! can't load there.

use serde::{Serialize, de::DeserializeOwned};
use std::mem::{size_of, size_of_val};

pub use reacs_derive::Save;

/// A type that can be stored in a save.
///
/// A type's bytes are its canonical encoding (see the [module docs](self)): the same state gives
/// the same bytes on every build and platform.
pub trait Save: Sized {
    /// `true` when a value's memory is its encoding, so values are saved and loaded by copying
    /// memory: every field is plain, in declaration order, with no padding, on a little-endian
    /// target.
    const PLAIN: bool;

    /// `true` when a plain type has fields that only allow some bit patterns, checked on load with
    /// [`check_all`](Self::check_all).
    const CHECKED: bool = false;

    /// Appends the value's encoding to `out`.
    fn save(&self, out: &mut Vec<u8>);

    /// Reads a value written by [`save`](Self::save).
    ///
    /// # Panics
    ///
    /// Panics if the bytes are corrupted.
    fn load(reader: &mut Reader<'_>) -> Self;

    /// For a checked plain type, returns `true` if every value in `bytes` is valid: `count` values
    /// `stride` bytes apart, this type's bytes starting `offset` bytes into each.
    #[inline]
    fn check_all(bytes: &[u8], count: usize, stride: usize, offset: usize) -> bool {
        let _ = (bytes, count, stride, offset);
        true
    }

    /// Appends the length of `items`, then each item: one memory copy for a plain type.
    #[inline]
    fn save_slice(items: &[Self], out: &mut Vec<u8>) {
        write_len(out, items.len());
        if Self::PLAIN {
            out.extend_from_slice(memory_of_slice(items));
        } else {
            for item in items {
                item.save(out);
            }
        }
    }

    /// Reads items written by [`save_slice`](Self::save_slice): one memory copy for a plain type.
    ///
    /// # Panics
    ///
    /// Panics if the bytes are corrupted.
    #[inline]
    fn load_vec(reader: &mut Reader<'_>) -> Vec<Self> {
        let len = reader.read_len();
        if !Self::PLAIN {
            let mut items = Vec::with_capacity(len.min(reader.remaining()));
            for _ in 0..len {
                items.push(Self::load(reader));
            }
            return items;
        }
        let byte_len = len
            .checked_mul(size_of::<Self>())
            .unwrap_or_else(|| corrupted("vector length"));
        let bytes = reader.take(byte_len);
        if Self::CHECKED && !Self::check_all(bytes, len, size_of::<Self>(), 0) {
            corrupted(std::any::type_name::<Self>());
        }
        let mut items = Vec::<Self>::with_capacity(len);
        // SAFETY: `items` has room for `len` values and `bytes` holds exactly `len` values' bytes;
        // they don't overlap. A plain type's memory is its encoding (`PLAIN`, checked at compile
        // time from its layout) and has no pointers, so any bytes are a valid value except in
        // checked fields, which `check_all` just validated.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                items.as_mut_ptr().cast::<u8>(),
                byte_len,
            );
            items.set_len(len);
        }
        items
    }
}

#[cold]
#[track_caller]
fn corrupted(what: &str) -> ! {
    panic!("Failed to load save, it's corrupted: bad {what}")
}

/// The memory of `values`, for plain types.
#[inline]
fn memory_of_slice<T: Save>(values: &[T]) -> &[u8] {
    debug_assert!(T::PLAIN);
    // SAFETY: the slice's `size_of_val` bytes are live for the returned borrow. Only called for
    // plain types, which have no padding (`PLAIN` checks it), so every byte is initialized.
    unsafe { std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), size_of_val(values)) }
}

/// The memory of `value`, for `#[derive(Save)]`.
///
/// Only for types without padding: plain types, enums without data (just their tag) and
/// `#[save(plain)]` fields.
#[doc(hidden)]
#[inline]
pub fn memory_of<T>(value: &T) -> &[u8] {
    // SAFETY: `value`'s `size_of` bytes are live for the returned borrow. Callers pass types
    // without padding, so every byte is initialized: plain types (`PLAIN` checks the layout),
    // enums without data (just their tag) and `#[save(plain)]` fields, whose author promises
    // plain data.
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

/// Appends a length as a little-endian `u64`.
#[inline]
pub fn write_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&(len as u64).to_le_bytes());
}

/// Appends `value`'s memory as it is: what `#[save(plain)]` does for a type from another crate.
///
/// Only for plain data: no pointers, no padding, every bit pattern valid. The bytes are the
/// value's own layout, so they match across platforms only if the type's layout does.
#[doc(hidden)]
#[inline]
pub fn save_plain<T: Copy>(value: &T, out: &mut Vec<u8>) {
    out.extend_from_slice(memory_of(value));
}

/// Lets postcard write straight into the save.
struct Append<'a>(&'a mut Vec<u8>);

impl postcard::ser_flavors::Flavor for Append<'_> {
    type Output = ();

    #[inline]
    fn try_push(&mut self, data: u8) -> postcard::Result<()> {
        self.0.push(data);
        Ok(())
    }

    #[inline]
    fn try_extend(&mut self, data: &[u8]) -> postcard::Result<()> {
        self.0.extend_from_slice(data);
        Ok(())
    }

    #[inline]
    fn finalize(self) -> postcard::Result<()> {
        Ok(())
    }
}

/// Appends `value` encoded with serde: what `#[save(serde)]` does for a type from another crate.
///
/// postcard's encoding ends on its own, so no length is written.
#[doc(hidden)]
#[inline]
pub fn save_serde<T: Serialize>(value: &T, out: &mut Vec<u8>) {
    postcard::serialize_with_flavor(value, Append(out)).expect("failed to encode a value");
}

/// Reads, in order, the values [`Save`] wrote.
pub struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    /// Starts reading `bytes` from the front.
    #[inline]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    /// Returns how many bytes are left.
    #[inline]
    pub fn remaining(&self) -> usize {
        self.bytes.len()
    }

    /// Returns the next `len` bytes and moves past them.
    ///
    /// # Panics
    ///
    /// Panics if fewer than `len` bytes are left.
    #[inline]
    pub fn take(&mut self, len: usize) -> &'a [u8] {
        if len > self.bytes.len() {
            panic!("Failed to load save, it's corrupted: it ends early");
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        head
    }

    /// Returns the next `N` bytes as an array and moves past them.
    ///
    /// # Panics
    ///
    /// Panics if fewer than `N` bytes are left.
    #[inline]
    pub fn take_array<const N: usize>(&mut self) -> [u8; N] {
        self.take(N).try_into().unwrap()
    }

    /// Reads a length written by [`write_len`].
    ///
    /// # Panics
    ///
    /// Panics if the bytes end early or the length doesn't fit a `usize`.
    #[inline]
    pub fn read_len(&mut self) -> usize {
        usize::try_from(u64::from_le_bytes(self.take_array()))
            .unwrap_or_else(|_| corrupted("length"))
    }

    /// Reads a value written by [`save_plain`].
    #[doc(hidden)]
    #[inline]
    pub fn load_plain<T: Copy>(&mut self) -> T {
        let bytes = self.take(size_of::<T>());
        // SAFETY: `bytes` holds `size_of::<T>()` bytes, read unaligned. They're a valid `T`
        // because `#[save(plain)]`'s author promises every bit pattern is.
        unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<T>()) }
    }

    /// Reads one value of a plain type by copying its memory, after checking it if it's checked.
    #[doc(hidden)]
    #[inline]
    pub fn load_memory<T: Save>(&mut self) -> T {
        debug_assert!(T::PLAIN);
        let bytes = self.take(size_of::<T>());
        if T::CHECKED && !T::check_all(bytes, 1, size_of::<T>(), 0) {
            corrupted(std::any::type_name::<T>());
        }
        // SAFETY: `bytes` holds `size_of::<T>()` bytes, read unaligned. A plain type's memory is
        // its encoding with no pointers, so any bytes are a valid value except in checked fields,
        // which were just checked.
        unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<T>()) }
    }

    /// Reads a value written by [`save_serde`].
    #[doc(hidden)]
    #[inline]
    pub fn load_serde<T: DeserializeOwned>(&mut self) -> T {
        match postcard::take_from_bytes(self.bytes) {
            Ok((value, rest)) => {
                self.bytes = rest;
                value
            }
            Err(_) => corrupted(std::any::type_name::<T>()),
        }
    }

    /// Checks that every byte was read.
    ///
    /// # Panics
    ///
    /// Panics if bytes are left over.
    pub fn finish(self) {
        if !self.bytes.is_empty() {
            panic!("Failed to load save, it's corrupted: it has unexpected trailing bytes");
        }
    }
}

/// Plain on little-endian targets: memory is little-endian there.
const LITTLE_ENDIAN: bool = cfg!(target_endian = "little");

macro_rules! numbers {
    ($($t:ty),*) => {$(
        impl Save for $t {
            const PLAIN: bool = LITTLE_ENDIAN;

            #[inline]
            fn save(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }

            #[inline]
            fn load(reader: &mut Reader<'_>) -> Self {
                Self::from_le_bytes(reader.take_array())
            }
        }
    )*};
}
numbers!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, f32, f64);

/// Saved as a `u64`, so it's plain only where it is one.
impl Save for usize {
    const PLAIN: bool = LITTLE_ENDIAN && size_of::<usize>() == 8;

    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        (*self as u64).save(out);
    }

    #[inline]
    fn load(reader: &mut Reader<'_>) -> Self {
        usize::try_from(u64::load(reader)).unwrap_or_else(|_| corrupted("usize, too big here"))
    }
}

/// Saved as an `i64`, so it's plain only where it is one.
impl Save for isize {
    const PLAIN: bool = LITTLE_ENDIAN && size_of::<isize>() == 8;

    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        (*self as i64).save(out);
    }

    #[inline]
    fn load(reader: &mut Reader<'_>) -> Self {
        isize::try_from(i64::load(reader)).unwrap_or_else(|_| corrupted("isize, too big here"))
    }
}

impl Save for bool {
    const PLAIN: bool = true;
    const CHECKED: bool = true;

    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }

    #[inline]
    fn load(reader: &mut Reader<'_>) -> Self {
        match reader.take_array::<1>() {
            [0] => false,
            [1] => true,
            _ => corrupted("bool"),
        }
    }

    #[inline]
    fn check_all(bytes: &[u8], count: usize, stride: usize, offset: usize) -> bool {
        (0..count).all(|i| bytes[i * stride + offset] <= 1)
    }
}

impl Save for char {
    const PLAIN: bool = LITTLE_ENDIAN;
    const CHECKED: bool = true;

    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        (*self as u32).save(out);
    }

    #[inline]
    fn load(reader: &mut Reader<'_>) -> Self {
        char::from_u32(u32::load(reader)).unwrap_or_else(|| corrupted("char"))
    }

    #[inline]
    fn check_all(bytes: &[u8], count: usize, stride: usize, offset: usize) -> bool {
        (0..count).all(|i| {
            let at = i * stride + offset;
            char::from_u32(u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())).is_some()
        })
    }
}

impl Save for () {
    const PLAIN: bool = true;

    #[inline]
    fn save(&self, _: &mut Vec<u8>) {}

    #[inline]
    fn load(_: &mut Reader<'_>) -> Self {}
}

impl<T: ?Sized> Save for std::marker::PhantomData<T> {
    const PLAIN: bool = true;

    #[inline]
    fn save(&self, _: &mut Vec<u8>) {}

    #[inline]
    fn load(_: &mut Reader<'_>) -> Self {
        Self
    }
}

/// Saves an enum without data, for `#[derive(Save)]`: its tag, little-endian.
#[doc(hidden)]
#[inline]
pub fn save_tag<T>(value: &T, out: &mut Vec<u8>) {
    let start = out.len();
    out.extend_from_slice(memory_of(value));
    if !LITTLE_ENDIAN {
        out[start..].reverse();
    }
}

/// Reads an enum without data saved by [`save_tag`], for `#[derive(Save)]`: returns the index of
/// the variant in `variants` whose tag it is.
///
/// # Panics
///
/// Panics if the tag is none of `variants`.
#[doc(hidden)]
#[inline]
pub fn load_tag<T>(reader: &mut Reader<'_>, variants: &[T]) -> usize {
    let mut bytes = reader.take(size_of::<T>()).to_vec();
    if !LITTLE_ENDIAN {
        bytes.reverse();
    }
    variants
        .iter()
        .position(|variant| memory_of(variant) == bytes.as_slice())
        .unwrap_or_else(|| corrupted(std::any::type_name::<T>()))
}

/// Checks the tags of enums without data, for `#[derive(Save)]`: each must be one of `variants`.
#[doc(hidden)]
#[inline]
pub fn check_tags<T>(
    variants: &[T],
    bytes: &[u8],
    count: usize,
    stride: usize,
    offset: usize,
) -> bool {
    let size = size_of::<T>();
    if size == 1 {
        let mut valid = [false; 256];
        for variant in variants {
            valid[memory_of(variant)[0] as usize] = true;
        }
        (0..count).all(|i| valid[bytes[i * stride + offset] as usize])
    } else {
        (0..count).all(|i| {
            let at = i * stride + offset;
            variants
                .iter()
                .any(|variant| memory_of(variant) == &bytes[at..at + size])
        })
    }
}

impl<T: Save, const N: usize> Save for [T; N] {
    const PLAIN: bool = T::PLAIN;
    const CHECKED: bool = T::CHECKED;

    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        if Self::PLAIN {
            out.extend_from_slice(memory_of_slice(self));
        } else {
            for item in self {
                item.save(out);
            }
        }
    }

    #[inline]
    fn load(reader: &mut Reader<'_>) -> Self {
        if Self::PLAIN {
            reader.load_memory()
        } else {
            std::array::from_fn(|_| T::load(reader))
        }
    }

    #[inline]
    fn check_all(bytes: &[u8], count: usize, stride: usize, offset: usize) -> bool {
        (0..N).all(|i| T::check_all(bytes, count, stride, offset + i * size_of::<T>()))
    }
}

macro_rules! tuples {
    ($(($($name:ident $index:tt),+))*) => {$(
        impl<$($name: Save),+> Save for ($($name,)+) {
            const PLAIN: bool = {
                let mut plain = true $(&& $name::PLAIN)+;
                let mut end = 0;
                $(
                    plain = plain && std::mem::offset_of!(Self, $index) == end;
                    end += size_of::<$name>();
                )+
                plain && size_of::<Self>() == end
            };
            const CHECKED: bool = false $(|| $name::CHECKED)+;

            #[inline]
            fn save(&self, out: &mut Vec<u8>) {
                if Self::PLAIN {
                    out.extend_from_slice(memory_of_slice(std::slice::from_ref(self)));
                } else {
                    $(self.$index.save(out);)+
                }
            }

            #[inline]
            fn load(reader: &mut Reader<'_>) -> Self {
                if Self::PLAIN {
                    reader.load_memory()
                } else {
                    ($($name::load(reader),)+)
                }
            }

            #[inline]
            fn check_all(bytes: &[u8], count: usize, stride: usize, offset: usize) -> bool {
                true $(&& $name::check_all(
                    bytes, count, stride, offset + std::mem::offset_of!(Self, $index),
                ))+
            }
        }
    )*};
}
tuples! {
    (A 0)
    (A 0, B 1)
    (A 0, B 1, C 2)
    (A 0, B 1, C 2, D 3)
    (A 0, B 1, C 2, D 3, E 4)
    (A 0, B 1, C 2, D 3, E 4, F 5)
}

/// A `u8` tag, `0` for `None` and `1` for `Some`, then the value.
impl<T: Save> Save for Option<T> {
    const PLAIN: bool = false;

    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        match self {
            None => out.push(0),
            Some(value) => {
                out.push(1);
                value.save(out);
            }
        }
    }

    #[inline]
    fn load(reader: &mut Reader<'_>) -> Self {
        match reader.take_array::<1>() {
            [0] => None,
            [1] => Some(T::load(reader)),
            _ => corrupted("Option"),
        }
    }
}

/// The length, then the items: one memory copy when they're plain.
impl<T: Save> Save for Vec<T> {
    const PLAIN: bool = false;

    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        T::save_slice(self, out);
    }

    #[inline]
    fn load(reader: &mut Reader<'_>) -> Self {
        T::load_vec(reader)
    }
}

impl<T: Save> Save for Box<T> {
    const PLAIN: bool = false;

    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        (**self).save(out);
    }

    #[inline]
    fn load(reader: &mut Reader<'_>) -> Self {
        Box::new(T::load(reader))
    }
}

/// The length in bytes, then the UTF-8 bytes.
impl Save for String {
    const PLAIN: bool = false;

    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        write_len(out, self.len());
        out.extend_from_slice(self.as_bytes());
    }

    #[inline]
    fn load(reader: &mut Reader<'_>) -> Self {
        let len = reader.read_len();
        std::str::from_utf8(reader.take(len))
            .unwrap_or_else(|_| corrupted("string"))
            .to_owned()
    }
}

/// Which rule a type is saved by (see the [module docs](self)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    /// Copied as memory.
    Plain,
    /// Copied as memory after its `bool`, `char` and enum fields are checked.
    PlainChecked,
    /// Written field by field.
    FieldByField,
}

impl Rule {
    /// Returns the rule `T` is saved by.
    pub const fn of<T: Save>() -> Rule {
        if !T::PLAIN {
            Rule::FieldByField
        } else if T::CHECKED {
            Rule::PlainChecked
        } else {
            Rule::Plain
        }
    }

    /// Returns the slower of two rules.
    pub const fn slower(self, other: Rule) -> Rule {
        if self as u8 >= other as u8 {
            self
        } else {
            other
        }
    }
}

impl std::fmt::Display for Rule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Rule::Plain => "plain",
            Rule::PlainChecked => "plain, checked",
            Rule::FieldByField => "field by field",
        })
    }
}

/// How long one resource takes to save and load, from
/// [`World::save_report`](crate::world::World::save_report).
#[derive(Clone, Debug)]
pub struct ResourceReport {
    /// What the resource stores.
    pub kind: crate::registry::Kind,
    /// The path of the type declared in game code: the resource, component or signal.
    pub path: &'static str,
    /// The rule its values are saved by. For components and signals, the rule of each stored
    /// value with reacs's bookkeeping around it.
    pub rule: Rule,
    /// The size of its saved bytes.
    pub bytes: u64,
    /// The median time to save it.
    pub save: std::time::Duration,
    /// The median time to load it.
    pub load: std::time::Duration,
}

/// How long each resource takes to save and load, slowest first, from
/// [`World::save_report`](crate::world::World::save_report).
///
/// Printing it shows a table. Resources under 1% of the total time are counted on one last line.
#[derive(Clone, Debug, Default)]
pub struct SaveReport {
    /// Every saved resource, by save and load time together, slowest first.
    pub resources: Vec<ResourceReport>,
}

impl SaveReport {
    /// Returns the size of every resource's saved bytes together.
    pub fn bytes(&self) -> u64 {
        self.resources.iter().map(|r| r.bytes).sum()
    }

    /// Returns the time to save every resource, one after another.
    pub fn save(&self) -> std::time::Duration {
        self.resources.iter().map(|r| r.save).sum()
    }

    /// Returns the time to load every resource, one after another.
    pub fn load(&self) -> std::time::Duration {
        self.resources.iter().map(|r| r.load).sum()
    }
}

fn milliseconds(time: std::time::Duration) -> String {
    format!("{:.2} ms", time.as_secs_f64() * 1e3)
}

fn size(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} KB", bytes as f64 / 1e3),
        _ => format!("{:.1} MB", bytes as f64 / 1e6),
    }
}

fn share(part: f64) -> String {
    if part < 0.01 {
        "<1%".into()
    } else {
        format!("{:.0}%", part * 100.0)
    }
}

impl std::fmt::Display for SaveReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total = (self.save() + self.load()).as_secs_f64();
        let part = |r: &ResourceReport| {
            if total > 0.0 {
                (r.save + r.load).as_secs_f64() / total
            } else {
                0.0
            }
        };
        writeln!(
            f,
            "save report: {}, save {}, load {}",
            size(self.bytes()),
            milliseconds(self.save()),
            milliseconds(self.load())
        )?;
        writeln!(f)?;
        writeln!(
            f,
            "{:>3}  {:>5}  {:>9}  {:>9}  {:>8}  {:<14}  type",
            "#", "share", "save", "load", "size", "rule"
        )?;
        let (shown, others): (Vec<_>, Vec<_>) =
            self.resources.iter().partition(|r| part(r) >= 0.01);
        for (rank, r) in shown.iter().enumerate() {
            writeln!(
                f,
                "{:>3}  {:>5}  {:>9}  {:>9}  {:>8}  {:<14}  {:<14}  {}",
                rank + 1,
                share(part(r)),
                milliseconds(r.save),
                milliseconds(r.load),
                size(r.bytes),
                r.rule,
                r.kind,
                r.path
            )?;
        }
        if !others.is_empty() {
            let save: std::time::Duration = others.iter().map(|r| r.save).sum();
            let load: std::time::Duration = others.iter().map(|r| r.load).sum();
            let bytes: u64 = others.iter().map(|r| r.bytes).sum();
            let part: f64 = others.iter().map(|r| part(r)).sum();
            let label = if others.len() == 1 { "other" } else { "others" };
            writeln!(
                f,
                "{:>3}  {:>5}  {:>9}  {:>9}  {:>8}  {:<14}  {} {label}",
                "",
                share(part),
                milliseconds(save),
                milliseconds(load),
                size(bytes),
                "",
                others.len()
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes<T: Save>(value: &T) -> Vec<u8> {
        let mut out = Vec::new();
        value.save(&mut out);
        out
    }

    fn round_trip<T: Save + PartialEq + std::fmt::Debug>(value: T) -> Vec<u8> {
        let encoded = bytes(&value);
        let mut reader = Reader::new(&encoded);
        assert_eq!(T::load(&mut reader), value);
        reader.finish();
        encoded
    }

    fn load<T: Save>(encoded: &[u8]) -> T {
        let mut reader = Reader::new(encoded);
        let value = T::load(&mut reader);
        reader.finish();
        value
    }

    // The expected bytes are constants, so these tests passing on every target is what shows the
    // encoding is the same everywhere (CI runs them on 64-bit and 32-bit).

    #[test]
    fn numbers_are_little_endian() {
        assert_eq!(round_trip(0x0102_0304u32), [4, 3, 2, 1]);
        assert_eq!(round_trip(-2i16), [0xfe, 0xff]);
        assert_eq!(round_trip(1.0f32), [0, 0, 0x80, 0x3f]);
        assert_eq!(round_trip(0x0102u128)[..3], [2, 1, 0]);
    }

    #[test]
    fn usize_and_isize_are_64_bit() {
        assert_eq!(round_trip(0x0102usize), [2, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(round_trip(-1isize), [0xff; 8]);
        assert_eq!(<usize as Save>::PLAIN, size_of::<usize>() == 8);
    }

    #[test]
    #[cfg(target_pointer_width = "32")]
    #[should_panic(expected = "too big here")]
    fn usize_too_big_for_the_target_panics() {
        load::<usize>(&u64::MAX.to_le_bytes());
    }

    #[test]
    fn small_types() {
        assert_eq!(round_trip(true), [1]);
        assert_eq!(round_trip(false), [0]);
        assert_eq!(round_trip('é'), [0xe9, 0, 0, 0]);
        assert_eq!(round_trip(()), []);
        assert_eq!(round_trip(std::marker::PhantomData::<String>), []);
    }

    #[test]
    fn containers() {
        assert_eq!(round_trip(Some(7u16)), [1, 7, 0]);
        assert_eq!(round_trip(None::<u16>), [0]);
        assert_eq!(
            round_trip(String::from("ab")),
            [2, 0, 0, 0, 0, 0, 0, 0, b'a', b'b']
        );
        assert_eq!(round_trip(Box::new(3u8)), [3]);
        assert_eq!(round_trip([1u16, 2]), [1, 0, 2, 0]);
        assert_eq!(
            round_trip(vec![1u16, 2]),
            [2, 0, 0, 0, 0, 0, 0, 0, 1, 0, 2, 0]
        );
        assert_eq!(
            round_trip(vec![String::from("a")]),
            [1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, b'a']
        );
    }

    #[test]
    fn tuples_are_fields_in_order_whatever_their_layout() {
        // Plain on little-endian targets: no padding, in order.
        assert_eq!(<(u32, u32)>::PLAIN, LITTLE_ENDIAN);
        assert_eq!(round_trip((1u32, 2u32)), [1, 0, 0, 0, 2, 0, 0, 0]);
        // Padded: field by field, no padding written.
        const { assert!(!<(u8, u64)>::PLAIN) };
        assert_eq!(round_trip((1u8, 2u64)), [1, 2, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(round_trip((1u64, 2u8)), [1, 0, 0, 0, 0, 0, 0, 0, 2]);
    }

    #[test]
    fn plain_and_field_by_field_vectors_give_the_same_bytes() {
        let plain = vec![(1u32, 2u32), (3, 4)];
        let mut by_field = Vec::new();
        write_len(&mut by_field, plain.len());
        for (a, b) in &plain {
            by_field.extend_from_slice(&a.to_le_bytes());
            by_field.extend_from_slice(&b.to_le_bytes());
        }
        assert_eq!(round_trip(plain), by_field);
    }

    #[test]
    fn checked_plain_vectors_are_checked() {
        const { assert!(<(bool, u8)>::PLAIN == LITTLE_ENDIAN && <(bool, u8)>::CHECKED) };
        assert_eq!(
            round_trip(vec![(true, 2u8)]),
            [1, 0, 0, 0, 0, 0, 0, 0, 1, 2]
        );
    }

    #[test]
    #[should_panic(expected = "corrupted")]
    fn bad_bool_in_a_plain_vector_panics() {
        load::<Vec<(bool, u8)>>(&[1, 0, 0, 0, 0, 0, 0, 0, 2, 2]);
    }

    #[test]
    #[should_panic(expected = "corrupted: bad bool")]
    fn bad_bool_panics() {
        load::<bool>(&[2]);
    }

    #[test]
    #[should_panic(expected = "corrupted: bad char")]
    fn bad_char_panics() {
        load::<char>(&0xd800u32.to_le_bytes());
    }

    #[test]
    #[should_panic(expected = "corrupted: bad Option")]
    fn bad_option_tag_panics() {
        load::<Option<u8>>(&[2, 0]);
    }

    #[test]
    #[should_panic(expected = "corrupted: bad string")]
    fn bad_utf8_panics() {
        load::<String>(&[1, 0, 0, 0, 0, 0, 0, 0, 0xff]);
    }

    #[test]
    #[should_panic(expected = "corrupted: it ends early")]
    fn truncated_vector_panics() {
        load::<Vec<u32>>(&[2, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]);
    }

    #[test]
    #[should_panic(expected = "corrupted")]
    fn huge_length_panics() {
        load::<Vec<u64>>(&[0xff; 8]);
    }

    #[test]
    #[should_panic(expected = "trailing bytes")]
    fn trailing_bytes_panic() {
        load::<u8>(&[1, 2]);
    }

    #[derive(Save, Clone, Copy, Default, PartialEq, Debug)]
    #[repr(C)]
    struct Plain {
        a: u32,
        b: i16,
        c: u16,
    }

    /// Small fields first: padded, and Rust may reorder it.
    #[derive(Save, Default, PartialEq, Debug)]
    struct Unordered {
        small: u8,
        big: u64,
        mid: u16,
    }

    #[derive(Save, Clone, Copy, Default, PartialEq, Debug)]
    #[repr(u8)]
    enum Team {
        #[default]
        Red,
        Green = 5,
        Blue,
    }

    #[derive(Save, Clone, Copy, Default, PartialEq, Debug)]
    #[repr(C)]
    struct Checked {
        team: Team,
        alive: bool,
        level: u16,
    }

    #[derive(Save, Clone, Copy, Default, PartialEq, Debug)]
    enum Order {
        #[default]
        Idle,
        Move {
            x: i32,
            y: i32,
        },
        Attack(u64),
    }

    #[derive(Save, Default, PartialEq, Debug)]
    struct Nested {
        id: u32,
        points: Vec<Plain>,
        name: String,
        order: Option<Order>,
    }

    #[derive(Save, Default, PartialEq, Debug)]
    #[repr(C)]
    struct Generic<T> {
        count: u32,
        value: T,
    }

    #[derive(Save, Default, PartialEq, Debug, Serialize, serde::Deserialize)]
    struct Attributes {
        #[save(plain)]
        wrapped: std::num::Wrapping<u32>,
        #[save(serde)]
        timeout: std::time::Duration,
        #[save(skip)]
        cache: u32,
        #[serde(skip)]
        other: u32,
        kept: u16,
    }

    #[derive(Save, Default, PartialEq, Debug)]
    struct Unit;

    #[test]
    fn plain_structs_save_their_fields_in_order() {
        const { assert!(<Plain as Save>::PLAIN == LITTLE_ENDIAN) };
        let value = Plain { a: 1, b: -2, c: 3 };
        assert_eq!(round_trip(value), [1, 0, 0, 0, 0xfe, 0xff, 3, 0]);

        let mut by_field = Vec::new();
        write_len(&mut by_field, 2);
        for _ in 0..2 {
            value.a.save(&mut by_field);
            value.b.save(&mut by_field);
            value.c.save(&mut by_field);
        }
        assert_eq!(round_trip(vec![value; 2]), by_field);
    }

    #[test]
    fn padded_or_reordered_structs_save_their_fields_in_order() {
        const { assert!(!<Unordered as Save>::PLAIN) };
        let value = Unordered {
            small: 1,
            big: 2,
            mid: 3,
        };
        assert_eq!(round_trip(value), [1, 2, 0, 0, 0, 0, 0, 0, 0, 3, 0]);
    }

    #[test]
    fn generic_structs_are_plain_only_without_padding() {
        const { assert!(<Generic<u32> as Save>::PLAIN == LITTLE_ENDIAN) };
        const { assert!(!<Generic<u8> as Save>::PLAIN) };
        assert_eq!(
            round_trip(Generic {
                count: 1,
                value: 2u8
            }),
            [1, 0, 0, 0, 2]
        );
    }

    #[test]
    fn enums_without_data_are_checked_plain() {
        const { assert!(<Team as Save>::PLAIN && <Team as Save>::CHECKED) };
        assert_eq!(round_trip(Team::Green), [5]);
        assert_eq!(round_trip(Team::Blue), [6]);

        const { assert!(<Checked as Save>::PLAIN == LITTLE_ENDIAN && <Checked as Save>::CHECKED) };
        let value = Checked {
            team: Team::Blue,
            alive: true,
            level: 0x0102,
        };
        assert_eq!(round_trip(value), [6, 1, 2, 1]);
        assert_eq!(
            round_trip(vec![value; 3])[8..],
            [6, 1, 2, 1, 6, 1, 2, 1, 6, 1, 2, 1]
        );
    }

    #[test]
    #[should_panic(expected = "corrupted")]
    fn bad_enum_tag_panics() {
        load::<Team>(&[1]);
    }

    #[test]
    #[should_panic(expected = "corrupted")]
    fn bad_enum_tag_in_a_plain_vector_panics() {
        load::<Vec<Checked>>(&[1, 0, 0, 0, 0, 0, 0, 0, 7, 1, 0, 0]);
    }

    #[test]
    #[should_panic(expected = "corrupted")]
    fn bad_bool_in_a_plain_struct_panics() {
        load::<Checked>(&[5, 2, 0, 0]);
    }

    #[test]
    fn enums_with_data_save_the_variant_index_then_its_fields() {
        const { assert!(!<Order as Save>::PLAIN) };
        assert_eq!(round_trip(Order::Idle), [0, 0, 0, 0]);
        assert_eq!(
            round_trip(Order::Move { x: 1, y: -1 }),
            [1, 0, 0, 0, 1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(
            round_trip(Order::Attack(7)),
            [2, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    #[should_panic(expected = "corrupted")]
    fn bad_variant_index_panics() {
        load::<Order>(&[3, 0, 0, 0]);
    }

    #[test]
    fn nested_types_round_trip() {
        let value = Nested {
            id: 9,
            points: vec![Plain { a: 1, b: 2, c: 3 }; 4],
            name: "squad".into(),
            order: Some(Order::Attack(3)),
        };
        let encoded = round_trip(value);
        assert_eq!(encoded.len(), 4 + (8 + 4 * 8) + (8 + 5) + (1 + 4 + 8));
    }

    #[test]
    fn field_attributes() {
        const { assert!(!<Attributes as Save>::PLAIN) };
        let value = Attributes {
            wrapped: std::num::Wrapping(0x0102_0304),
            timeout: std::time::Duration::from_secs(2),
            cache: 5,
            other: 6,
            kept: 7,
        };
        let encoded = bytes(&value);
        let mut serde = Vec::new();
        save_serde(&value.timeout, &mut serde);
        assert_eq!(encoded[..4], [4, 3, 2, 1]);
        assert_eq!(encoded[4..encoded.len() - 2], serde);
        assert_eq!(encoded[encoded.len() - 2..], [7, 0]);
        let loaded: Attributes = load(&encoded);
        assert_eq!(
            loaded,
            Attributes {
                cache: 0,
                other: 0,
                ..value
            }
        );
    }

    #[test]
    fn rules() {
        assert_eq!(Rule::of::<u32>(), Rule::Plain);
        assert_eq!(Rule::of::<bool>(), Rule::PlainChecked);
        assert_eq!(Rule::of::<String>(), Rule::FieldByField);
        assert_eq!(Rule::Plain.slower(Rule::PlainChecked), Rule::PlainChecked);
        assert_eq!(Rule::FieldByField.slower(Rule::Plain), Rule::FieldByField);
    }

    fn row(
        kind: crate::registry::Kind,
        path: &'static str,
        rule: Rule,
        bytes: u64,
        save_us: u64,
        load_us: u64,
    ) -> ResourceReport {
        ResourceReport {
            kind,
            path,
            rule,
            bytes,
            save: std::time::Duration::from_micros(save_us),
            load: std::time::Duration::from_micros(load_us),
        }
    }

    #[test]
    fn report_table_folds_resources_under_one_percent() {
        use crate::registry::Kind;
        let report = SaveReport {
            resources: vec![
                row(
                    Kind::Resource,
                    "game::Units",
                    Rule::FieldByField,
                    8_000_000,
                    110,
                    5_700,
                ),
                row(
                    Kind::Signal,
                    "game::Move",
                    Rule::Plain,
                    16_000_104,
                    200,
                    200,
                ),
                row(
                    Kind::Component,
                    "game::Position",
                    Rule::Plain,
                    12_582_920,
                    160,
                    150,
                ),
                row(Kind::SignalCursors, "game::Move", Rule::Plain, 40, 0, 1),
                row(Kind::Resource, "game::Rng", Rule::Plain, 8, 1, 0),
            ],
        };
        assert_eq!(
            report.to_string(),
            "save report: 36.6 MB, save 0.47 ms, load 6.05 ms

  #  share       save       load      size  rule            type
  1    89%    0.11 ms    5.70 ms    8.0 MB  field by field  resource        game::Units
  2     6%    0.20 ms    0.20 ms   16.0 MB  plain           signal          game::Move
  3     5%    0.16 ms    0.15 ms   12.6 MB  plain           component       game::Position
       <1%    0.00 ms    0.00 ms      48 B                  2 others
"
        );
    }

    #[test]
    fn unit_structs_save_nothing() {
        const { assert!(<Unit as Save>::PLAIN) };
        assert_eq!(round_trip(Unit), []);
    }
}
