use reacs_derive::param;

use crate::{direct_map::DirectMap, prelude::*, registry::get_registry_entry};
use std::cell::{Ref, RefMut};

#[param]
pub struct Component<C: Default + Copy + Hashed> {
    pub map: Resource<DirectMap<C>>,
}

impl World {
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

#[cfg(test)]
mod tests {
    use crate::{prelude::*, schedule::Mut};

    #[component]
    struct IntComponent(pub i32);

    #[test]
    fn component_mut_inserts() {
        const KEY: usize = 18;

        #[system]
        fn write(mut c_int: Mut<Component<IntComponent>>) {
            c_int.insert(KEY, IntComponent(967));
        }
        #[system]
        fn check(c: Component<IntComponent>) {
            assert_eq!(c.map.get(KEY).map(|c| c.0), Some(967));
        }

        let mut world = World::new();

        write.into_system().run(&mut world);
        check.into_system().run(&mut world);
    }

    #[test]
    fn component_mut_removes() {
        const KEY: usize = 7;

        #[system]
        fn remove(mut c_int: Mut<Component<IntComponent>>) {
            c_int.remove(KEY);
        }
        #[system]
        fn check(c: Component<IntComponent>) {
            assert!(c.map.get(KEY).is_none());
        }

        let mut world = World::new();
        world.set_component(KEY, IntComponent(344));

        remove.into_system().run(&mut world);
        check.into_system().run(&mut world);
    }
}
