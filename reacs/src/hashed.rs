//! Stable ids for types and systems, the same on every build and machine.

pub use reacs_derive::Hashed;

/// A type with a stable 128-bit id.
///
/// Saves, recordings and the type registry identify types by this id, so it's the same on every
/// build and machine. Derive it with `#[derive(DeriveHashed)]` from the prelude;
/// `#[resource]`, `#[component]` and `#[signal]` already derive it.
#[diagnostic::on_unimplemented(
    message = "`{Self}` isn't declared with #[resource], #[component] or #[signal]",
    label = "not a reacs type",
    note = "if this is `Tick`: it's a param, not a resource, so take `tick: Tick` to read the tick (nothing can write it)"
)]
pub trait Hashed {
    /// The type's id: a hash of its module path and name, mixed with its generic parameters' ids.
    const TYPE_UUID: u128;
}

const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
/// The 128-bit FNV-1a prime that type ids are built with.
pub const PRIME: u128 = 0x0000000001000000000000000000013B;

const fn fnv(mut h: u128, bytes: &[u8]) -> u128 {
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u128;
        h = h.wrapping_mul(PRIME);
        i += 1;
    }
    h
}

/// Returns the id of the item `name` in module `module`: a 128-bit FNV-1a hash of `module::name`.
///
/// `#[derive(Hashed)]` builds type ids with it, and `#[system]` builds system ids, so an id is the
/// same on every build and machine as long as the item keeps its name and module.
///
/// # Examples
///
/// ```
/// use reacs::hashed::hash_path;
///
/// assert_eq!(hash_path("game", "Health"), hash_path("game", "Health"));
/// assert_ne!(hash_path("game", "Health"), hash_path("game::units", "Health"));
/// ```
pub const fn hash_path(module: &str, name: &str) -> u128 {
    let h = fnv(OFFSET, module.as_bytes());
    let h = fnv(h, b"::");
    fnv(h, name.as_bytes())
}
