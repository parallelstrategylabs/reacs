pub use reacs_derive::Hashed;

pub trait Hashed {
    const TYPE_UUID: u128;
}

const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
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

pub const fn hash_path(module: &str, name: &str) -> u128 {
    let h = fnv(OFFSET, module.as_bytes());
    let h = fnv(h, b"::");
    fnv(h, name.as_bytes())
}
