use std::{any::Any, collections::HashMap, ptr::addr_of_mut};
use std::collections::hash_map::Entry as MapEntry;

#[derive(Debug)]
pub struct Entry {
    pub type_name: &'static str,
    pub serialize: fn(&dyn Any) -> Vec<u8>,
    pub deserialize: fn(&[u8]) -> Box<dyn Any>,
}

static mut REGISTRY: Option<HashMap<u128, Entry>> = None;

pub fn register_entry<T>(id: u128)
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + 'static,
{
    unsafe {
        let ptr = addr_of_mut!(REGISTRY);
        let map = (*ptr).get_or_insert_with(HashMap::new);
        match map.entry(id) {
            MapEntry::Occupied(e) => panic!(
                "id collision: `{}` and `{}` share id {id:#034x}",
                e.get().type_name,
                std::any::type_name::<T>(),
            ),
            MapEntry::Vacant(slot) => {
                slot.insert(Entry {
                    type_name: std::any::type_name::<T>(),
                    serialize: |v| bincode::serialize(v.downcast_ref::<T>().unwrap()).unwrap(),
                    deserialize: |b| Box::new(bincode::deserialize::<T>(b).unwrap()),
                });
            }
        }
    }
}

pub fn get_registry_entry(id: u128) -> Option<&'static Entry> {
    unsafe {
        let ptr = addr_of_mut!(REGISTRY);
        (*ptr).as_ref()?.get(&id)
    }
}

#[cfg(test)]

mod tests {
    use crate::{direct_map::DirectMap, prelude::*, registry::get_registry_entry};

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
        let entry = get_registry_entry(DirectMap::<IntComponent>::TYPE_UUID);
        assert!(entry.is_some(), "Component registration failed!");
    }
}
