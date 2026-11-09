use std::{any::Any, cell::RefCell, ptr::addr_of_mut};

use crate::{index_map::IndexMap, state_hash::hash_value};

#[derive(Debug)]
pub struct RegistryEntry {
    pub type_name: &'static str,
    pub serialize: fn(&dyn Any) -> Vec<u8>,
    pub deserialize: fn(&[u8]) -> Box<dyn Any>,
    pub(crate) restore: fn(&[u8]) -> Box<RefCell<dyn Any>>,
    pub(crate) hash: fn(&dyn Any) -> u64,
}

fn decode<T: for<'de> serde::Deserialize<'de>>(bytes: &[u8]) -> T {
    bincode::deserialize(bytes).unwrap_or_else(|error| {
        panic!(
            "Failed to deserialize `{}`: {error}",
            std::any::type_name::<T>()
        )
    })
}

pub static mut REGISTRY: Option<IndexMap<u128, RegistryEntry>> = None;

pub fn register_entry<T>(id: u128)
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + 'static,
{
    unsafe {
        let ptr = addr_of_mut!(REGISTRY);
        let map = (*ptr).get_or_insert_with(IndexMap::new);
        if let Some(existing) = map.get(&id) {
            panic!(
                "id collision: `{}` and `{}` share id {id:#034x}",
                existing.type_name,
                std::any::type_name::<T>(),
            );
        }
        map.insert(
            id,
            RegistryEntry {
                type_name: std::any::type_name::<T>(),
                serialize: |v| bincode::serialize(v.downcast_ref::<T>().unwrap()).unwrap(),
                deserialize: |b| Box::new(decode::<T>(b)),
                restore: |b| Box::new(RefCell::new(decode::<T>(b))),
                hash: |v| hash_value(v.downcast_ref::<T>().unwrap()),
            },
        );
    }
}

pub fn registered_ids() -> Vec<u128> {
    unsafe {
        let ptr = addr_of_mut!(REGISTRY);
        (*ptr)
            .as_ref()
            .map_or_else(Vec::new, |map| map.iter().map(|entry| entry.0).collect())
    }
}

pub fn get_registry_entry(id: u128) -> Option<&'static RegistryEntry> {
    unsafe {
        let ptr = addr_of_mut!(REGISTRY);
        (*ptr).as_ref()?.get(&id)
    }
}

#[cfg(test)]
mod tests {
    use crate::{component::ComponentMap, prelude::*, registry::get_registry_entry};

    #[resource]
    struct IntResource(pub u64);

    #[test]
    fn resource_auto_registers_in_registry() {
        let entry = get_registry_entry(IntResource::TYPE_UUID);
        assert!(entry.is_some(), "Resource registration failed!");
    }

    #[test]
    fn resource_serialize_deserialize_roundtrip() {
        let entry = get_registry_entry(IntResource::TYPE_UUID).unwrap();
        let original = IntResource(42);

        let bytes = (entry.serialize)(&original as &dyn core::any::Any);
        let restored = (entry.deserialize)(&bytes);

        let restored = restored
            .downcast::<IntResource>()
            .expect("Downcast failed, incorrect type!");

        assert_eq!(restored.0, original.0, "Resource deserialization failed!");
    }

    #[component]
    struct IntComponent(pub i32);

    #[test]
    fn component_auto_registers_in_registry() {
        let entry = get_registry_entry(ComponentMap::<IntComponent>::TYPE_UUID);
        assert!(entry.is_some(), "Component registration failed!");
    }
}
