pub use reacs_derive::Hashed;
pub trait Hashed {
    const TYPE_UUID: u128;
}