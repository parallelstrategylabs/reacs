use core::any::TypeId;
use std::{any::Any, cell::RefCell, collections::HashMap, ptr::addr_of_mut};

extern crate self as reacs;

// 1. Re-exporta a macro para que ela possa ser usada com #[derive(HashedTypeDef)]
pub use reacs_derive::Hashed;

use crate::{
    resource::{Resource, ResourceMut},
    schedule::Schedule,
    world::World,
};

pub trait Hashed {
    const TYPE_UUID: u128;
}

pub mod registry;
pub mod resource;
pub mod schedule;
pub mod world;

pub fn test_system1(param1: Resource<Speed>, param2: Resource<Speed>) {}
pub fn test_system2(param2: ResourceMut<Speed>) {}

#[derive(Default)]
pub struct Speed {
    pub value: f32,
}

pub fn test_system3(mut param1: ResourceMut<Speed>) {
    param1.value += 1.0;
}

pub fn dangerous_system(mov: Resource<Speed>, mut mov_mut: ResourceMut<Speed>) {
    mov_mut.value += 1.0;
}

#[cfg(test)]
mod tests {
    use std::hash::{DefaultHasher, Hash, Hasher};

    use reacs_derive::resource;

    use crate::registry::get_registry_entry;

use super::*;

    #[resource]
    struct TestResource(pub f64);
    
    #[test]
    fn resource_registers_in_registry() {
        let entry = get_registry_entry(TestResource::TYPE_UUID);
        assert!(entry.is_some(), "Resource registration failed!");
    }

    #[test]
    fn resource_serialize_deserialize_roundtrip() {
        let entry = get_registry_entry(TestResource::TYPE_UUID).unwrap();
        let original = TestResource(42.5);

        let bytes = (entry.serialize)(&original as &dyn Any);
        let restored = (entry.deserialize)(&bytes);

        let restored = restored
            .downcast::<TestResource>()
            .expect("Downcast failed, incorrect type!");

        assert_eq!(restored.0, original.0, "Resource deserialization failed!");
    }

    #[test]
    fn world_inserts_resource() {
        let mut world = World::new();
        world.insert_resource::<TestResource>();
        assert!(world.has_resource::<TestResource>(), "Resource insertion failed!");
    }
}
