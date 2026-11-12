use std::{
    any::Any,
    cell::RefCell,
    sync::{Mutex, OnceLock, PoisonError},
};

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

pub(crate) struct Registry<T> {
    building: Mutex<Building<T>>,
    frozen: OnceLock<T>,
}

struct Building<T> {
    closed: bool,
    value: Option<T>,
}

impl<T: Default> Registry<T> {
    pub(crate) const fn new() -> Self {
        Self {
            building: Mutex::new(Building {
                closed: false,
                value: None,
            }),
            frozen: OnceLock::new(),
        }
    }

    pub(crate) fn add(&self, name: &str, add: impl FnOnce(&mut T)) {
        let mut building = self.building.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            !building.closed,
            "`{name}` was registered after reacs started running: registration only happens before \
             main, from the code reacs macros generate"
        );
        add(building.value.get_or_insert_with(T::default));
    }

    pub(crate) fn get(&self) -> &T {
        self.frozen.get_or_init(|| {
            let mut building = self.building.lock().unwrap_or_else(PoisonError::into_inner);
            building.closed = true;
            building.value.take().unwrap_or_default()
        })
    }
}

static REGISTRY: Registry<IndexMap<u128, RegistryEntry>> = Registry::new();

#[doc(hidden)]
pub fn register_entry<T>(id: u128)
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + 'static,
{
    let type_name = std::any::type_name::<T>();
    REGISTRY.add(type_name, |map| {
        if let Some(existing) = map.get(&id) {
            panic!(
                "id collision: `{}` and `{type_name}` share id {id:#034x}",
                existing.type_name,
            );
        }
        map.insert(
            id,
            RegistryEntry {
                type_name,
                serialize: |v| bincode::serialize(v.downcast_ref::<T>().unwrap()).unwrap(),
                deserialize: |b| Box::new(decode::<T>(b)),
                restore: |b| Box::new(RefCell::new(decode::<T>(b))),
                hash: |v| hash_value(v.downcast_ref::<T>().unwrap()),
            },
        );
    });
}

pub fn registered_ids() -> Vec<u128> {
    REGISTRY.get().iter().map(|entry| entry.0).collect()
}

pub fn get_registry_entry(id: u128) -> Option<&'static RegistryEntry> {
    REGISTRY.get().get(&id)
}

#[cfg(test)]
mod tests {
    use crate::{component::ComponentMap, prelude::*, registry::get_registry_entry};

    #[test]
    fn a_registry_keeps_what_was_added_before_the_first_read() {
        let registry: super::Registry<Vec<u32>> = super::Registry::new();
        registry.add("a", |list| list.push(1));
        registry.add("b", |list| list.push(2));
        assert_eq!(registry.get(), &[1, 2]);
        assert_eq!(registry.get(), &[1, 2]);
    }

    #[test]
    fn an_empty_registry_reads_as_empty() {
        let registry: super::Registry<Vec<u32>> = super::Registry::new();
        assert!(registry.get().is_empty());
    }

    #[test]
    #[should_panic(expected = "`late` was registered after reacs started running")]
    fn registering_after_the_first_read_panics() {
        let registry: super::Registry<Vec<u32>> = super::Registry::new();
        registry.add("early", |list| list.push(1));
        registry.get();
        registry.add("late", |list| list.push(2));
    }

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
