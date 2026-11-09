use reacs_derive::{Hashed, param};
use serde::{Deserialize, Serialize};

use crate::{
    direct_map::DirectMap, entity::Entity, prelude::*, registry::get_registry_entry,
    schedule::Write,
};
use std::cell::{Ref, RefMut};

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Slot<C> {
    generation: u32,
    value: C,
}

#[derive(Default, Hashed, Serialize, Deserialize)]
pub struct ComponentMap<C: Copy + Default> {
    slots: DirectMap<Slot<C>>,
}

impl<C: Copy + Default> ComponentMap<C> {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn has(&self, entity: Entity) -> bool {
        self.get(entity).is_some()
    }

    #[inline]
    pub fn get(&self, entity: Entity) -> Option<&C> {
        self.slots
            .get(entity.index() as usize)
            .filter(|slot| slot.generation == entity.generation())
            .map(|slot| &slot.value)
    }

    #[inline]
    pub fn get_mut(&mut self, entity: Entity) -> Option<&mut C> {
        self.slots
            .get_mut(entity.index() as usize)
            .filter(|slot| slot.generation == entity.generation())
            .map(|slot| &mut slot.value)
    }

    #[inline]
    pub(crate) fn insert(&mut self, entity: Entity, value: C) {
        let index = entity.index() as usize;
        let generation = entity.generation();
        if self
            .slots
            .get(index)
            .is_some_and(|s| s.generation > generation)
        {
            return;
        }
        self.slots.insert(index, Slot { generation, value });
    }

    #[inline]
    pub(crate) fn remove(&mut self, entity: Entity) {
        if self.has(entity) {
            self.slots.remove(entity.index() as usize);
        }
    }
}

#[param]
pub struct Component<C: Default + Copy + Hashed> {
    map: Resource<ComponentMap<C>>,
}

impl<C: 'static + Default + Copy + Hashed> Component<'_, C, Write> {
    #[inline]
    pub fn get_mut(&mut self, entity: Entity) -> Option<&mut C> {
        self.map.get_mut(entity)
    }

    #[inline]
    pub fn insert(&mut self, entity: Entity, value: C) {
        self.map.insert(entity, value);
    }

    #[inline]
    pub fn remove(&mut self, entity: Entity) {
        self.map.remove(entity);
    }
}

impl World {
    #[inline]
    pub(crate) fn init_component<C: 'static + Copy + Default + Hashed>(&mut self) {
        let id = ComponentMap::<C>::TYPE_UUID;
        get_registry_entry(id).unwrap_or_else(|| {
            let type_name = std::any::type_name::<C>();
            panic!("Component `{type_name}` not registered, try using #[component] {type_name}!")
        });

        if !self.has_resource::<ComponentMap<C>>() {
            self.insert_resource(ComponentMap::<C>::new());
        }
    }

    #[inline]
    pub fn get_component_map<C: 'static + Copy + Default + Hashed>(
        &self,
    ) -> Ref<'_, ComponentMap<C>> {
        self.get_resource::<ComponentMap<C>>().unwrap_or_else(|| {
            panic!(
                "Component `{}` not initialized, call init_component first!",
                std::any::type_name::<C>()
            )
        })
    }

    #[inline]
    pub(crate) fn get_component_map_mut<C: 'static + Copy + Default + Hashed>(
        &mut self,
    ) -> RefMut<'_, ComponentMap<C>> {
        self.get_resource_mut::<ComponentMap<C>>()
            .unwrap_or_else(|| {
                panic!(
                    "Component `{}` not initialized, call init_component first!",
                    std::any::type_name::<C>()
                )
            })
    }

    #[inline]
    pub(crate) fn set_component<C: 'static + Copy + Default + Hashed>(
        &mut self,
        entity: Entity,
        value: C,
    ) {
        self.get_component_map_mut::<C>().insert(entity, value);
    }

    #[inline]
    pub(crate) fn remove_component<C: 'static + Copy + Default + Hashed>(
        &mut self,
        entity: Entity,
    ) {
        self.get_component_map_mut::<C>().remove(entity);
    }

    #[inline]
    pub fn get_component<C: 'static + Copy + Default + Hashed>(&self, entity: Entity) -> Option<C> {
        self.get_component_map().get(entity).copied()
    }

    #[inline]
    pub fn has_component<C: 'static + Copy + Default + Hashed>(&self, entity: Entity) -> bool {
        self.get_component_map::<C>().has(entity)
    }
}

#[cfg(test)]
mod tests {
    use crate::{entity::Entity, prelude::*, schedule::Mut};

    #[component]
    struct IntComponent(pub i32);

    #[component]
    struct OtherComponent(pub i32);

    #[signal]
    struct Hit(pub Entity);

    #[test]
    fn component_mut_inserts() {
        const KEY: Entity = Entity::new(18, 1);

        #[system]
        fn write(mut c_int: Mut<Component<IntComponent>>) {
            c_int.insert(KEY, IntComponent(967));
        }
        #[system]
        fn check_inserted(c: Component<IntComponent>) {
            assert_eq!(c.get(KEY).map(|c| c.0), Some(967));
        }

        let mut world = World::new();

        write.into_system().run(&mut world);
        check_inserted.into_system().run(&mut world);
    }

    #[test]
    fn component_mut_removes() {
        const KEY: Entity = Entity::new(7, 1);

        #[system]
        fn remove(mut c_int: Mut<Component<IntComponent>>) {
            c_int.remove(KEY);
        }
        #[system]
        fn check_removed(c: Component<IntComponent>) {
            assert!(c.get(KEY).is_none());
        }

        let mut world = World::new();
        world.init_component::<IntComponent>();
        world.set_component(KEY, IntComponent(344));

        remove.into_system().run(&mut world);
        check_removed.into_system().run(&mut world);
    }

    #[test]
    fn component_mut_changes_value_in_place() {
        let entity = Entity::new(3, 1);
        let mut world = World::new();
        world.init_component::<IntComponent>();
        world.set_component(entity, IntComponent(1));

        world
            .get_component_map_mut::<IntComponent>()
            .get_mut(entity)
            .unwrap()
            .0 = 2;

        assert_eq!(world.get_component::<IntComponent>(entity).unwrap().0, 2);
    }

    fn world_with_reused_index() -> (World, Entity, Entity) {
        let mut world = World::new();
        world.init_param::<Mut<EntityManager>>();
        world.init_component::<IntComponent>();

        let old = world.entity_manager().spawn();
        world.set_component(old, IntComponent(1));
        world.entity_manager().despawn(old);
        let new = world.entity_manager().spawn();
        assert_eq!(old.index(), new.index());

        (world, old, new)
    }

    #[test]
    fn new_entity_does_not_inherit_components() {
        let (world, _, new) = world_with_reused_index();

        assert!(!world.has_component::<IntComponent>(new));
        assert!(world.get_component::<IntComponent>(new).is_none());
    }

    #[test]
    fn old_entity_cannot_overwrite_new_entity() {
        let (mut world, old, new) = world_with_reused_index();
        world.set_component(new, IntComponent(2));

        world.set_component(old, IntComponent(3));

        assert_eq!(world.get_component::<IntComponent>(new).unwrap().0, 2);
    }

    #[test]
    fn old_entity_cannot_remove_from_new_entity() {
        let (mut world, old, new) = world_with_reused_index();
        world.set_component(new, IntComponent(2));

        world.remove_component::<IntComponent>(old);

        assert_eq!(world.get_component::<IntComponent>(new).unwrap().0, 2);
    }

    #[test]
    fn old_entity_inserting_into_dead_slot_does_not_reach_new_entity() {
        let (mut world, old, new) = world_with_reused_index();

        world.set_component(old, IntComponent(3));

        assert!(!world.has_component::<IntComponent>(new));
    }

    #[test]
    fn new_entity_overwrites_dead_slot() {
        let (mut world, _, new) = world_with_reused_index();

        world.set_component(new, IntComponent(2));

        assert_eq!(world.get_component::<IntComponent>(new).unwrap().0, 2);
    }

    #[test]
    fn system_changes_components_and_despawns_while_looping_signals() {
        #[system]
        fn on_hit(
            mut hits: Mut<Signal<Hit>>,
            mut ints: Mut<Component<IntComponent>>,
            mut others: Mut<Component<OtherComponent>>,
            mut em: Mut<EntityManager>,
        ) {
            for &Hit(target) in hits.receive() {
                ints.remove(target);
                others.insert(target, OtherComponent(5));
                assert!(!ints.has(target));
                assert_eq!(others.get(target).unwrap().0, 5);

                em.despawn(target);
                assert!(!em.is_alive(target));

                let next = em.spawn();
                assert_eq!(next.index(), target.index());
                assert!(!others.has(next));
            }
        }

        let mut world = World::new();
        on_hit.into_system().init(&mut world);

        let target = world.entity_manager().spawn();
        world.set_component(target, IntComponent(1));
        world.signal::<Hit>().emit(Hit(target));

        on_hit.into_system().run(&mut world);
    }
}
