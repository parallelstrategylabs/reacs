//! The registry of every type declared with a reacs macro, by id.

use std::{
    any::Any,
    cell::RefCell,
    sync::{Mutex, OnceLock, PoisonError},
};

use crate::{
    index_map::IndexMap,
    save::{Reader, Save},
    state_hash::hash_value,
};

/// What a registered type stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A `#[resource]`.
    Resource,
    /// Every value of a `#[component]`.
    Component,
    /// The signals of a `#[signal]` type waiting to be delivered.
    Signal,
    /// Where each system is in receiving a `#[signal]` type.
    SignalCursors,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Kind::Resource => "resource",
            Kind::Component => "component",
            Kind::Signal => "signal",
            Kind::SignalCursors => "signal cursors",
        })
    }
}

/// What reacs knows about a registered type, found by its id with [`get_registry_entry`].
///
/// `#[resource]`, `#[component]` and `#[signal]` register their types before `main`. Tools built on
/// recordings use these entries to name and decode the resources a save or a hash list refers to.
#[derive(Debug)]
pub struct RegistryEntry {
    /// The type's full name, as [`std::any::type_name`] gives it.
    pub type_name: &'static str,
    /// What the type stores.
    pub kind: Kind,
    /// The path of the type declared in game code: the resource itself, or the component or
    /// signal type whose storage this is.
    pub path: &'static str,
    /// The rule each stored value is saved by: the resource's, or for components and signals,
    /// each value's with reacs's bookkeeping around it.
    pub rule: crate::save::Rule,
    /// Encodes a value of the type, passed as `&dyn Any`, the way saves store it.
    ///
    /// # Panics
    ///
    /// Panics if the value isn't of the entry's type.
    pub serialize: fn(&dyn Any) -> Vec<u8>,
    /// Decodes bytes written by [`serialize`](Self::serialize) into a boxed value of the type.
    ///
    /// # Panics
    ///
    /// Panics if the bytes don't decode as the type.
    pub deserialize: fn(&[u8]) -> Box<dyn Any>,
    pub(crate) save: fn(&dyn Any, &mut Vec<u8>),
    pub(crate) restore: fn(&[u8]) -> Box<RefCell<dyn Any>>,
    pub(crate) hash: fn(&dyn Any) -> u64,
}

/// Reads a value that must fill `bytes` exactly.
fn load_all<T: Save>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    let value = T::load(&mut reader);
    reader.finish();
    value
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

/// Registers a type stored in worlds. Called by the code the reacs macros generate.
///
/// `kind`, `path` and `rule` describe it in [`World::save_report`](crate::world::World::save_report).
#[doc(hidden)]
pub fn register_entry<T>(id: u128, kind: Kind, path: &'static str, rule: crate::save::Rule)
where
    T: Save + serde::Serialize + 'static,
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
                kind,
                path,
                rule,
                serialize: |v| {
                    let mut out = Vec::new();
                    v.downcast_ref::<T>().unwrap().save(&mut out);
                    out
                },
                deserialize: |b| Box::new(load_all::<T>(b)),
                save: |v, out| v.downcast_ref::<T>().unwrap().save(out),
                restore: |b| Box::new(RefCell::new(load_all::<T>(b))),
                hash: |v| hash_value(v.downcast_ref::<T>().unwrap()),
            },
        );
    });
}

/// Returns the id of every registered type, the ids saves and resource hashes use.
///
/// Look one up with [`get_registry_entry`]. The first call closes registration, as reading any
/// registry does.
pub fn registered_ids() -> Vec<u128> {
    REGISTRY.get().iter().map(|entry| entry.0).collect()
}

/// Returns the registered type with id `id`, or `None` if no type has it.
///
/// Ids are the types' [`TYPE_UUID`](crate::hashed::Hashed::TYPE_UUID), as stored in saves,
/// [`World::resource_hashes`](crate::world::World::resource_hashes) and recordings.
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
