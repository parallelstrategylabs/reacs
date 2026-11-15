//! A reactive, signal-driven ECS on a deterministic, tick-based simulation kernel.
//!
//! reacs runs game simulations, like those behind RTS and strategy games, one fixed tick at a
//! time. Its pieces:
//!
//! - **Entities** are ids ([`Entity`](entity::Entity)), spawned and despawned through
//!   [`EntityManager`](entity::EntityManager).
//! - **Components** are small, plain data attached to entities ([`#[component]`](prelude::component)),
//!   reached by entity in O(1).
//! - **Resources** are single values shared by the world ([`#[resource]`](prelude::resource)).
//! - **Signals** are messages between systems ([`#[signal]`](prelude::signal)), lasting one tick or a
//!   window of ticks.
//! - **Your own types** used as fields, like an enum, are declared with
//!   [`#[data]`](prelude::data) so they can be saved.
//! - **Systems** are functions ([`#[system]`](prelude::system)) that run once per tick, one after
//!   another.
//! - The [`World`](world::World) holds all of it and runs one tick per [`step`](world::World::step).
//!
//! Everything most games need is in the [`prelude`].
//!
//! # Signals instead of queries
//!
//! reacs has no queries: no way to visit every entity with a given set of components. A system
//! reacts to the signals it receives and touches the entities they name, reaching their components
//! by [`Entity`](entity::Entity). Work that happens every tick, like movement, is a signal sent
//! once with a window and delivered every tick until it ends or is cancelled. Signals are the only
//! thing reacs iterates, so a tick costs what happens in it, not how big the world is.
//!
//! # Determinism
//!
//! The same start state and the same inputs give the same results, tick for tick and byte for
//! byte, on Linux, Windows and macOS, on x86-64 and ARM64, in debug and release builds. Saves,
//! replays and lockstep networking rely on it. reacs keeps it by design, and CI checks it across
//! systems on every release:
//!
//! - The host, the program around the world, writes into a world only once, in
//!   [`World::build`](world::World::build). After that, the only way in is input signals
//!   ([`#[signal(input)]`](prelude::signal)) sent with [`World::input`](world::World::input).
//! - All state lives in resources, components and signals, which are saved, loaded and hashed.
//!   Local resources ([`#[resource(local)]`](prelude::resource)) are the exception, and must only
//!   hold caches.
//! - Spawning always reuses the lowest free index, and systems run in a fixed order.
//!
//! reacs can only promise its own part: a game is deterministic only if its systems are. In
//! systems, and in anything they call:
//!
//! - Store maps as [`IndexMap`](index_map::IndexMap), never `HashMap` or `HashSet`, whose iteration
//!   order changes between runs.
//! - Don't call float math library functions like `sin`, `cos`, `atan2`, `exp`, `ln`, `powf` or
//!   `powi`: they can return different bits on different systems. `+`, `-`, `*`, `/`, `sqrt`,
//!   `abs` and casts are exact everywhere.
//! - Don't read anything from outside the world: system time, unseeded randomness, environment
//!   variables, files. Keep a seeded random generator in a resource, and send outside data as input
//!   signals.
//! - Don't order anything by address or `TypeId`, and don't rely on the size of `usize`.
//! - Check that dependencies, like a physics crate, follow the same rules.
//!
//! [`World::save`](world::World::save) and [`World::load`](world::World::load) store and restore a
//! whole world. [`World::to_recorder`](world::World::to_recorder) records a session's inputs, and
//! [`World::to_playback`](world::World::to_playback) replays it, checking that every tick matches.
//!
//! # Examples
//!
//! A unit set on fire: a windowed signal deals damage every tick until the unit dies, then a
//! plain signal reports the death.
//!
//! ```rust,standalone_crate
//! use reacs::prelude::*;
//!
//! #[component]
//! struct Health(i32);
//!
//! // Safe entity lifecycle: a signal gets dropped if its target is despawned.
//! #[signal(entity)]
//! struct Burn {
//!     target: Entity,
//!     damage: i32,
//! }
//!
//! // Plain signal: news about an entity must outlive it.
//! #[signal]
//! struct Died(Entity);
//!
//! #[resource]
//! struct Graveyard {
//!     dead: Vec<Entity>,
//! }
//!
//! // Where work takes place: a system only awakes when it receives a signal, no observers required.
//! // Cost scales with signal count, not entities.
//! #[system]
//! fn burn(
//!     mut burns: Mut<Receiver<Burn>>,
//!     mut health: Mut<Component<Health>>,
//!     mut entities: Mut<EntityManager>,
//!     mut died: Mut<Sender<Died>>,
//! ) {
//!     // Batch logic execution: helps the CPU by processing the same signal one after another.
//!     for burn in burns.receive() {
//!         // Friendly fast lookups: random component access is cheap and common.
//!         let Some(hp) = health.get_mut(burn.target) else { continue };
//!         hp.0 -= burn.damage;
//!         if hp.0 <= 0 {
//!             entities.despawn(burn.target);
//!             died.emit(Died(burn.target));
//!         }
//!     }
//! }
//!
//! // Avoid the 1-frame delay: system execution is sequential, and can be customized.
//! #[system(run_after(burn))]
//! fn bury(mut deaths: Mut<Receiver<Died>>, mut graveyard: Mut<Resource<Graveyard>>) {
//!     for died in deaths.receive() {
//!         graveyard.dead.push(died.0);
//!     }
//! }
//!
//! fn main() {
//!     let mut unit = Entity::default();
//!     // Decentralized registration: systems and resources are automatically added to a new world.
//!     let mut world = World::build(|setup| {
//!         unit = setup.spawn();
//!         setup.set_component(unit, Health(10));
//!         // Declarative signal emission: start next tick, until cancelled or the unit dies.
//!         setup.signal::<Burn>().send(Burn { target: unit, damage: 4 }, (In(1), ..));
//!     });
//!
//!     for _ in 0..5 {
//!         world.step();
//!     }
//!
//!     assert!(!world.entities().is_alive(unit));
//!     assert_eq!(world.get_resource::<Graveyard>().unwrap().dead, [unit]);
//! }
//! ```

#![warn(missing_docs)]

extern crate self as reacs;

pub mod bit_set;
pub(crate) mod codec;
pub mod component;
pub mod direct_map;
pub mod entity;
pub(crate) mod free_list;
pub mod hashed;
pub mod index_map;
pub mod record;
pub mod registry;
pub mod resource;
pub mod schedule;
pub mod signal;
pub mod state_hash;
pub mod tick;
pub mod world;

/// Runs the README's examples as doctests, so they can't drift from the API.
#[cfg(doctest)]
#[doc = include_str!("../../README.md")]
pub struct ReadmeDoctests;

#[doc(hidden)]
pub mod __private {
    pub use crate::signal::{wake_init, wake_name, wake_pending};
    pub use ctor;
    pub use serde;
}

/// The types and macros most game code needs.
///
/// ```
/// use reacs::prelude::*;
/// ```
pub mod prelude {
    pub use crate::component::Component;
    pub use crate::entity::{Entity, EntityManager};
    pub use crate::hashed::Hashed;
    pub use crate::index_map::IndexMap;
    pub use crate::resource::{LocalResource, Resource};
    pub use crate::schedule::{
        IntoSystem, Mut, Setup, SystemEntry, SystemGraph, SystemParam, WorldAccess, system_graph,
    };
    pub use crate::signal::{
        At, For, In, InputSignal, IntoWindow, Receiver, Sender, Signal, SignalType, To,
    };
    pub use crate::tick::Tick;
    pub use crate::world::World;

    #[doc(inline)]
    pub use reacs_derive::{
        Hashed as DeriveHashed, component, data, param, resource, signal, system, world,
    };
}

#[cfg(test)]
pub(crate) const fn test_size(native: usize, miri: usize) -> usize {
    if cfg!(miri) { miri } else { native }
}
