use std::{
    any::Any,
    cell::{Ref, RefCell, RefMut},
    collections::HashMap,
};

use crate::{direct_map::DirectMap, hashed::Hashed, registry::get_registry_entry};
pub struct World {
    pub(crate) table: HashMap<u128, u32>,
    pub(crate) storage: Vec<Box<RefCell<dyn Any>>>,
}

impl World {
    #[inline]
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
            storage: Vec::new(),
        }
    }

    #[inline]
    pub fn insert_resource<R: 'static + Hashed>(&mut self, resource: R) {
        let id = R::TYPE_UUID;
        if let Some(&index) = self.table.get(&id) {
            self.storage[index as usize] = Box::new(RefCell::new(resource));
        } else {
            self.table.insert(R::TYPE_UUID, self.storage.len() as u32);
            self.storage.push(Box::new(RefCell::new(resource)));
        }
    }

    #[inline]
    pub fn init_resource<R: 'static + Default + Hashed>(&mut self) {
        if !self.has_resource::<R>() {
            self.insert_resource(R::default());
        }
    }

    #[inline]
    pub fn remove_resource<R: 'static + Hashed>(&mut self) -> bool {
        let Some(index) = self.table.remove(&R::TYPE_UUID) else {
            return false;
        };
        self.storage.swap_remove(index as usize);

        let old_last = self.storage.len() as u32;
        if index != old_last {
            for slot in self.table.values_mut() {
                if *slot == old_last {
                    *slot = index;
                    break;
                }
            }
        }
        true
    }

    #[inline]
    pub fn has_resource<R: 'static + Hashed>(&self) -> bool {
        let id = R::TYPE_UUID;
        self.table.contains_key(&id)
    }

    #[inline]
    pub fn get_resource<R: 'static + Hashed>(&self) -> Option<Ref<'_, R>> {
        let id = R::TYPE_UUID;
        let index = *self.table.get(&id)?;
        let storage_cell = self.storage.get(index as usize)?;
        let borrow = storage_cell.borrow();
        Ref::filter_map(borrow, |any| any.downcast_ref::<R>()).ok()
    }

    #[inline]
    pub fn get_resource_mut<R: 'static + Hashed>(&mut self) -> Option<RefMut<'_, R>> {
        self.get_resource_mut_silent::<R>()
    }

    #[inline]
    pub(crate) fn get_resource_mut_silent<R: 'static + Hashed>(&self) -> Option<RefMut<'_, R>> {
        let id = R::TYPE_UUID;
        let index = *self.table.get(&id)?;
        let storage_cell = self.storage.get(index as usize)?;

        let borrow = storage_cell.borrow_mut();
        RefMut::filter_map(borrow, |any| any.downcast_mut::<R>()).ok()
    }

    #[inline]
    pub fn init_component<C: 'static + Copy + Default + Hashed>(&mut self) {
        let id = DirectMap::<C>::TYPE_UUID;
        get_registry_entry(id).unwrap_or_else(|| {
            let type_name = std::any::type_name::<C>();
            panic!("Component `{type_name}` not registered, try using #[component] {type_name}!")
        });

        if !self.has_resource::<DirectMap<C>>() {
            self.insert_resource(DirectMap::<C>::new());
        }
    }

    #[inline]
    pub fn get_component_map<C: 'static + Copy + Default + Hashed>(&self) -> Ref<'_, DirectMap<C>> {
        self.get_resource::<DirectMap<C>>().unwrap_or_else(|| {
            panic!(
                "Component `{}` not initialized, call init_component first!",
                std::any::type_name::<C>()
            )
        })
    }

    #[inline]
    pub fn get_component_map_mut<C: 'static + Copy + Default + Hashed>(
        &mut self,
    ) -> RefMut<'_, DirectMap<C>> {
        self.get_resource_mut::<DirectMap<C>>().unwrap_or_else(|| {
            panic!(
                "Component `{}` not initialized, call init_component first!",
                std::any::type_name::<C>()
            )
        })
    }

    #[inline]
    pub fn set_component<C: 'static + Copy + Default + Hashed>(&mut self, key: usize, value: C) {
        self.get_component_map_mut::<C>().insert(key, value);
    }

    #[inline]
    pub fn remove_component<C: 'static + Copy + Default + Hashed>(&mut self, key: usize) {
        self.get_component_map_mut::<C>().remove(key);
    }

    #[inline]
    pub fn get_component<C: 'static + Copy + Default + Hashed>(&self, key: usize) -> Option<C> {
        self.get_component_map().get(key).copied()
    }

    #[inline]
    pub fn has_component<C: 'static + Copy + Default + Hashed>(&self, key: usize) -> bool {
        self.get_component_map::<C>().has(key)
    }
}

pub struct StoredSerializer {
    pub serialize_fn: Box<dyn FnMut(&World)>,
}

#[cfg(test)]

mod tests {
    use crate::prelude::*;

    #[resource]
    struct IntResource(pub u64);

    #[test]
    fn world_inserts_resource() {
        let mut world = World::new();
        world.init_resource::<IntResource>();

        assert!(
            world.has_resource::<IntResource>(),
            "Resource insertion failed!"
        );
    }

    #[test]
    fn world_gets_resource() {
        let mut world = World::new();
        world.insert_resource(IntResource(78));

        let value = world
            .get_resource::<IntResource>()
            .expect("Resource not initialized!");

        assert_eq!((*value).0, 78);
    }

    #[resource]
    struct OtherResource(pub bool);

    #[resource]
    struct ThirdResource(pub i32);

    #[test]
    fn remove_resource_clears_presence() {
        let mut world = World::new();
        world.init_resource::<IntResource>();

        world.remove_resource::<IntResource>();

        assert!(!world.has_resource::<IntResource>());
    }

    #[test]
    fn remove_first_resource_keeps_others_reachable() {
        let mut world = World::new();
        world.insert_resource(IntResource(1));
        world.insert_resource(OtherResource(true));
        world.insert_resource(ThirdResource(3));

        world.remove_resource::<IntResource>();

        assert!(world.get_resource::<OtherResource>().unwrap().0);
        assert_eq!(world.get_resource::<ThirdResource>().unwrap().0, 3);
    }

    #[test]
    fn remove_middle_resource_keeps_others_reachable() {
        let mut world = World::new();
        world.insert_resource(IntResource(1));
        world.insert_resource(OtherResource(true));
        world.insert_resource(ThirdResource(3));

        world.remove_resource::<OtherResource>();

        assert_eq!(world.get_resource::<IntResource>().unwrap().0, 1);
        assert_eq!(world.get_resource::<ThirdResource>().unwrap().0, 3);
    }

    #[test]
    fn remove_last_resource_keeps_others_reachable() {
        let mut world = World::new();
        world.insert_resource(IntResource(1));
        world.insert_resource(OtherResource(true));
        world.insert_resource(ThirdResource(3));

        world.remove_resource::<ThirdResource>();

        assert_eq!(world.get_resource::<IntResource>().unwrap().0, 1);
        assert!(world.get_resource::<OtherResource>().unwrap().0);
    }

    #[component]
    struct IntComponent(pub i32);

    #[test]
    fn world_sets_component() {
        const KEY: usize = 18;
        const VALUE: i32 = 967;

        let mut world = World::new();
        world.init_component::<IntComponent>();
        assert!(!world.has_component::<IntComponent>(KEY));

        world.set_component(KEY, IntComponent(VALUE));
        assert!(world.has_component::<IntComponent>(KEY));

        assert_eq!(world.get_component::<IntComponent>(KEY).unwrap().0, VALUE);
    }

    #[test]
    fn world_removes_component() {
        const KEY: usize = 7;

        let mut world = World::new();
        world.init_component::<IntComponent>();
        assert!(!world.has_component::<IntComponent>(KEY));

        world.set_component(KEY, IntComponent(344));
        assert!(world.has_component::<IntComponent>(KEY));

        world.remove_component::<IntComponent>(KEY);
        assert!(!world.has_component::<IntComponent>(KEY));
    }

    #[test]
    fn init_resource_keeps_existing_value() {
        let mut world = World::new();
        world.insert_resource(IntResource(7));

        world.init_resource::<IntResource>();

        assert_eq!(world.get_resource::<IntResource>().unwrap().0, 7);
    }

    #[test]
    fn init_component_keeps_existing_components() {
        let mut world = World::new();
        world.init_component::<IntComponent>();
        world.set_component(1, IntComponent(5));

        world.init_component::<IntComponent>();

        assert_eq!(world.get_component::<IntComponent>(1).unwrap().0, 5);
    }

    #[test]
    #[should_panic(expected = "not initialized")]
    fn get_component_panics_when_not_initialized() {
        let world = World::new();
        world.get_component::<IntComponent>(1);
    }
}
