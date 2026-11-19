//! [`Rng`], a seeded random number generator that gives the same numbers on every platform.

use std::ops::{Range, RangeInclusive};

use serde::{Deserialize, Serialize};

const GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;

#[inline(always)]
const fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A seeded random number generator: the same seed gives the same numbers, in the same order, on
/// every platform and build.
///
/// It's SplitMix64: 8 bytes of state, plain data that is `Copy`, saved and hashed like any other
/// field, small enough for a component. Keep one in a resource or a component, seeded in
/// [`World::build`](crate::world::World::build) or from an input signal, never from the time.
/// Every number drawn moves the state on, so a save holds exactly where the sequence is, and a
/// loaded world draws the same numbers as the original.
///
/// One generator in a resource gives numbers in the order systems draw them. When the order
/// isn't fixed, or when a system's work will be split across threads, derive a generator from
/// what the number is for with [`keyed`](Self::keyed), like a tick and an entity: the result no
/// longer depends on who asked first.
///
/// Not for cryptography.
///
/// # Examples
///
/// ```
/// use reacs::prelude::*;
///
/// let mut rng = Rng::new(42);
/// let roll = rng.range(1..=6);
/// assert!((1..=6).contains(&roll));
///
/// let mut again = Rng::new(42);
/// assert_eq!(again.range(1..=6), roll);
/// ```
///
/// A generator in a resource, seeded by the host:
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[resource]
/// struct Loot {
///     rng: Rng,
///     drops: Vec<u32>,
/// }
///
/// #[signal(input)]
/// struct OpenChest;
///
/// #[system]
/// fn open(mut chests: Mut<Receiver<OpenChest>>, mut loot: Mut<Resource<Loot>>) {
///     for _ in chests.receive() {
///         let gold = loot.rng.range(10..50);
///         loot.drops.push(gold);
///     }
/// }
///
/// fn main() {
///     let mut world = World::build(|setup| {
///         setup.insert_resource(Loot { rng: Rng::new(7), drops: Vec::new() });
///     });
///     world.input(OpenChest);
///     world.step();
///     assert_eq!(world.get_resource::<Loot>().unwrap().drops.len(), 1);
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, crate::save::Save)]
pub struct Rng {
    state: u64,
}

impl Default for Rng {
    /// A generator seeded with 0.
    fn default() -> Self {
        Self::new(0)
    }
}

impl Rng {
    /// Creates a generator from a seed.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Creates a generator from a seed and the keys of what the numbers are for, like a tick and
    /// an [`Entity`](crate::entity::Entity)'s [`to_bits`](crate::entity::Entity::to_bits).
    ///
    /// The same seed and keys always give the same generator, and different keys give unrelated
    /// ones. So a system can draw an entity's numbers without a shared generator: the result
    /// depends only on the seed and keys, not on the order entities are handled in, which keeps
    /// it deterministic when that order changes or the work is split across threads. Keys are
    /// order-sensitive: `[1, 2]` and `[2, 1]` give different generators.
    ///
    /// # Examples
    ///
    /// ```
    /// use reacs::prelude::*;
    ///
    /// let seed = 99;
    /// let (tick, unit) = (120, 7);
    /// let dodge = Rng::keyed(seed, &[tick, unit]).chance(0.25);
    /// assert_eq!(Rng::keyed(seed, &[tick, unit]).chance(0.25), dodge);
    /// ```
    pub const fn keyed(seed: u64, keys: &[u64]) -> Self {
        let mut state = mix(seed.wrapping_add(GAMMA));
        let mut i = 0;
        while i < keys.len() {
            state = mix(state.wrapping_add(GAMMA) ^ keys[i]);
            i += 1;
        }
        Self { state }
    }

    /// Returns the next 64 random bits.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GAMMA);
        mix(self.state)
    }

    /// Returns the next 32 random bits.
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Returns `true` or `false`, each half the time.
    #[inline]
    pub fn bool(&mut self) -> bool {
        self.next_u64() >> 63 == 1
    }

    /// Returns `true` with probability `p`: never for 0 or less, always for 1 or more.
    #[inline]
    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }

    /// Returns a number in `[0, 1)`, a multiple of 2⁻²⁴.
    #[inline]
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 * (1.0 / (1u32 << 24) as f32)
    }

    /// Returns a number in `[0, 1)`, a multiple of 2⁻⁵³.
    #[inline]
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Returns a number in `range`, every value equally likely.
    ///
    /// Takes `a..b` and `a..=b` of any integer type, and `a..b` of `f32` or `f64`. Integers are
    /// drawn without bias. A `usize` range draws the same numbers on 32-bit and 64-bit
    /// platforms.
    ///
    /// # Panics
    ///
    /// Panics if the range is empty, or if a float range isn't finite.
    ///
    /// # Examples
    ///
    /// ```
    /// use reacs::prelude::*;
    ///
    /// let mut rng = Rng::new(3);
    /// let damage = rng.range(5..=8);
    /// let angle = rng.range(0.0..360.0f32);
    /// let offset = rng.range(-10i32..10);
    /// assert!((5..=8).contains(&damage) && (0.0..360.0).contains(&angle) && offset.abs() <= 10);
    /// ```
    #[inline]
    pub fn range<T, R: RngRange<T>>(&mut self, range: R) -> T {
        range.sample(self)
    }

    /// Returns a uniformly chosen element of `items`, or `None` if it's empty.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        Some(&items[self.below(items.len() as u64) as usize])
    }

    /// Puts `items` in a random order, every order equally likely.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }

    /// Returns a number in `[0, n)` without bias (Lemire's method). `n` must not be 0.
    #[inline]
    fn below(&mut self, n: u64) -> u64 {
        let mut product = self.next_u64() as u128 * n as u128;
        if (product as u64) < n {
            let threshold = n.wrapping_neg() % n;
            while (product as u64) < threshold {
                product = self.next_u64() as u128 * n as u128;
            }
        }
        (product >> 64) as u64
    }
}

/// A range [`Rng::range`] can draw from: `a..b` and `a..=b` of integers, `a..b` of `f32` and
/// `f64`.
#[diagnostic::on_unimplemented(
    message = "`Rng::range` can't draw from `{Self}`",
    note = "use `a..b` or `a..=b` of an integer type, or `a..b` of `f32` or `f64`"
)]
pub trait RngRange<T> {
    /// Draws one value from the range.
    fn sample(self, rng: &mut Rng) -> T;
}

macro_rules! int_ranges {
    ($($int:ty => $unsigned:ty),* $(,)?) => {$(
        impl RngRange<$int> for Range<$int> {
            #[inline]
            fn sample(self, rng: &mut Rng) -> $int {
                assert!(self.start < self.end, "Rng::range: empty range {:?}", self);
                let span = self.end.wrapping_sub(self.start) as $unsigned as u64;
                self.start.wrapping_add(rng.below(span) as $int)
            }
        }

        impl RngRange<$int> for RangeInclusive<$int> {
            #[inline]
            fn sample(self, rng: &mut Rng) -> $int {
                let (start, end) = self.into_inner();
                assert!(start <= end, "Rng::range: empty range {start:?}..={end:?}");
                let span = (end.wrapping_sub(start) as $unsigned as u64).wrapping_add(1);
                let offset = if span == 0 { rng.next_u64() } else { rng.below(span) };
                start.wrapping_add(offset as $int)
            }
        }
    )*};
}

int_ranges! {
    u8 => u8, u16 => u16, u32 => u32, u64 => u64, usize => usize,
    i8 => u8, i16 => u16, i32 => u32, i64 => u64, isize => usize,
}

macro_rules! float_ranges {
    ($($float:ident),*) => {$(
        impl RngRange<$float> for Range<$float> {
            #[inline]
            fn sample(self, rng: &mut Rng) -> $float {
                assert!(
                    self.start < self.end && self.start.is_finite() && self.end.is_finite(),
                    "Rng::range: empty or non-finite range {:?}",
                    self
                );
                loop {
                    let value = self.start + (self.end - self.start) * rng.$float();
                    if value < self.end {
                        return value;
                    }
                }
            }
        }
    )*};
}

float_ranges!(f32, f64);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::save::Save;

    #[test]
    fn matches_the_reference_splitmix64() {
        let mut rng = Rng::new(1234567);
        let first: Vec<u64> = (0..5).map(|_| rng.next_u64()).collect();
        assert_eq!(
            first,
            [
                6457827717110365317,
                3203168211198807973,
                9817491932198370423,
                4593380528125082431,
                16408922859458223821,
            ]
        );
    }

    #[test]
    fn keyed_is_stable_and_spreads_keys() {
        assert_eq!(Rng::keyed(5, &[1, 2]), Rng::keyed(5, &[1, 2]));
        assert_ne!(Rng::keyed(5, &[1, 2]), Rng::keyed(5, &[2, 1]));
        assert_ne!(Rng::keyed(5, &[0]), Rng::keyed(5, &[0, 0]));
        assert_ne!(Rng::keyed(5, &[]), Rng::keyed(6, &[]));
        let mut seen: Vec<u64> = (0..1000)
            .map(|key| Rng::keyed(5, &[key]).next_u64())
            .collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 1000);
    }

    #[test]
    fn ranges_stay_inside_and_reach_both_ends() {
        let mut rng = Rng::new(9);
        let mut seen = [false; 6];
        for _ in 0..1000 {
            let roll = rng.range(1..=6u8);
            assert!((1..=6).contains(&roll));
            seen[roll as usize - 1] = true;
        }
        assert!(seen.iter().all(|&s| s));

        for _ in 0..1000 {
            assert!((-3..3).contains(&rng.range(-3i32..3)));
            assert!((-3.0..3.0).contains(&rng.range(-3.0f32..3.0)));
            assert!((0.0..1.0).contains(&rng.f64()));
        }
        assert_eq!(rng.range(7..8u64), 7);
        assert_eq!(rng.range(i8::MIN..=i8::MIN), i8::MIN);
        rng.range(i64::MIN..=i64::MAX);
        rng.range(0..=u64::MAX);
    }

    #[test]
    fn integer_ranges_are_unbiased() {
        let mut rng = Rng::new(11);
        let mut counts = [0u32; 3];
        for _ in 0..30_000 {
            counts[rng.range(0..3usize)] += 1;
        }
        assert!(
            counts.iter().all(|&c| (9_500..10_500).contains(&c)),
            "{counts:?}"
        );
    }

    #[test]
    fn usize_draws_match_u64() {
        let (mut a, mut b) = (Rng::new(4), Rng::new(4));
        for n in 1..200u64 {
            assert_eq!(a.range(0..n as usize) as u64, b.range(0..n));
        }
    }

    #[test]
    #[should_panic(expected = "empty range")]
    fn empty_range_panics() {
        Rng::new(0).range(3..3);
    }

    #[test]
    fn shuffle_and_pick() {
        let mut rng = Rng::new(1);
        let mut items: Vec<u32> = (0..50).collect();
        rng.shuffle(&mut items);
        assert_ne!(items, (0..50).collect::<Vec<_>>());
        items.sort_unstable();
        assert_eq!(items, (0..50).collect::<Vec<_>>());

        assert_eq!(rng.pick::<u32>(&[]), None);
        assert!(items.contains(rng.pick(&items).unwrap()));
    }

    #[test]
    fn chance_extremes() {
        let mut rng = Rng::new(2);
        assert!((0..100).all(|_| !rng.chance(0.0)));
        assert!((0..100).all(|_| rng.chance(1.0)));
    }

    #[test]
    fn saves_its_state() {
        let mut rng = Rng::new(77);
        rng.next_u64();
        let mut bytes = Vec::new();
        rng.save(&mut bytes);
        assert_eq!(bytes.len(), 8);
        let mut loaded = Rng::load(&mut crate::save::Reader::new(&bytes));
        assert_eq!(loaded.next_u64(), rng.next_u64());
    }
}
