use std::marker::PhantomData;

use reacs_derive::resource;

use crate::{
    schedule::{Mode, ParamInit, ParamWorld, Read, SystemParam, conflict},
    world::World,
};

/// The saved tick counter.
///
/// Only [`World::step`] advances it and [`World::load`] restores it. Game code can't name it, so
/// it reads the tick through [`Tick`] or [`World::tick`] and can never write it.
#[resource]
pub(crate) struct TickCount(pub u64);

/// System param that reads the current tick: how many times [`World::step`] has run.
///
/// The tick starts at 0. Every system in a step sees the same tick, and the step adds one at its
/// end. Signal params also give it with `tick()`, and the host reads it with [`World::tick`].
///
/// The tick can't be changed by game code: `Tick` has no `Mut` form, and the counter behind it
/// isn't public. Only [`World::step`] advances it and [`World::load`] restores it, so signal
/// windows and recordings can rely on it.
///
/// # Examples
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[resource]
/// struct Seen {
///     ticks: Vec<u64>,
/// }
///
/// #[system]
/// fn note(tick: Tick, mut seen: Mut<Resource<Seen>>) {
///     seen.ticks.push(tick.get());
/// }
///
/// fn main() {
///     let mut world = World::new();
///     world.step();
///     world.step();
///     assert_eq!(world.get_resource::<Seen>().unwrap().ticks, [0, 1]);
///     assert_eq!(world.tick(), 2);
/// }
/// ```
pub struct Tick<'w, M: Mode = Read> {
    tick: u64,
    _world: PhantomData<(&'w World, M)>,
}

impl<M: Mode> Tick<'_, M> {
    /// Returns the current tick.
    #[inline]
    pub fn get(&self) -> u64 {
        self.tick
    }
}

impl<M: Mode> SystemParam for Tick<'_, M> {
    type Item<'w> = Tick<'w, M>;

    fn init(world: ParamInit<'_>) {
        world.0.init_resource::<TickCount>();
    }

    fn fetch<'w>(world: ParamWorld<'w>) -> Tick<'w, M> {
        let tick = world
            .0
            .get_resource::<TickCount>()
            .unwrap_or_else(|| conflict::<TickCount>())
            .0;
        Tick {
            tick,
            _world: PhantomData,
        }
    }
}

impl World {
    /// Returns the current tick: how many times [`step`](Self::step) has run, counting the steps
    /// restored by [`load`](Self::load).
    ///
    /// # Examples
    ///
    /// ```rust,standalone_crate
    /// use reacs::prelude::*;
    ///
    /// fn main() {
    ///     let mut world = World::new();
    ///     assert_eq!(world.tick(), 0);
    ///     world.step();
    ///     assert_eq!(world.tick(), 1);
    /// }
    /// ```
    pub fn tick(&self) -> u64 {
        self.get_resource::<TickCount>().map_or(0, |tick| tick.0)
    }
}
