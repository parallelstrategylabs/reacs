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
pub mod world;

pub mod prelude {
    pub use crate::component::{Component, ComponentMut};
    pub use crate::hashed::Hashed;
    pub use crate::resource::{Resource, ResourceMut};
    pub use crate::schedule::{IntoSystem, SystemParam};
    pub use crate::world::World;

    pub use reacs_derive::{Hashed as DeriveHashed, component, resource, system};
}
