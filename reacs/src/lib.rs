use reacs_derive::resource;

extern crate self as reacs;

pub mod bit_set;
pub mod component;
pub mod direct_map;
pub mod entity;
pub mod free_list;
pub mod hashed;
pub mod registry;
pub mod resource;
pub mod schedule;
pub mod signal;
pub mod world;

#[doc(hidden)]
pub mod __private {
    pub use ctor;
    pub use serde;
}

pub mod prelude {
    pub use crate::component::Component;
    pub use crate::entity::{Entity, EntityManager};
    pub use crate::hashed::Hashed;
    pub use crate::resource::Resource;
    pub use crate::schedule::{
        IntoSystem, Mut, SystemEntry, SystemGraph, SystemParam, WorldAccess, system_graph,
    };
    pub use crate::signal::{Signal, SignalType};
    pub use crate::world::World;

    pub use reacs_derive::{
        Hashed as DeriveHashed, component, param, resource, signal, system, world,
    };
}

#[resource]
pub struct Tick(pub u64);
