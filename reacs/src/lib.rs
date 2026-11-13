use reacs_derive::resource;

extern crate self as reacs;

pub mod bit_set;
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
pub mod world;

#[doc(hidden)]
pub mod __private {
    pub use crate::signal::{wake_init, wake_name, wake_pending};
    pub use ctor;
    pub use serde;
}

pub mod prelude {
    pub use crate::Tick;
    pub use crate::component::Component;
    pub use crate::entity::{Entity, EntityManager};
    pub use crate::hashed::Hashed;
    pub use crate::index_map::IndexMap;
    pub use crate::resource::{LocalResource, Resource};
    pub use crate::schedule::{
        IntoSystem, Mut, SystemEntry, SystemGraph, SystemParam, WorldAccess, system_graph,
    };
    pub use crate::signal::{
        At, For, In, InputSignal, IntoWindow, Receiver, Sender, Signal, SignalType, To,
    };
    pub use crate::world::World;

    pub use reacs_derive::{
        Hashed as DeriveHashed, component, data, param, resource, signal, system, world,
    };
}

#[resource]
pub struct Tick(pub u64);

#[cfg(test)]
pub(crate) const fn test_size(native: usize, miri: usize) -> usize {
    if cfg!(miri) { miri } else { native }
}
