use std::ops::{Deref, DerefMut};

use reacs_derive::{param, resource};
use serde::{Deserialize, Serialize};

use crate::{
    component::{ComponentInfo, live_components},
    direct_map::DirectMap,
    free_list::FreeList,
    resource::Resource,
    schedule::{Mut, WorldAccess, Write},
    world::World,
};

#[derive(
    Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Entity {
    bits: u64,
}

impl Entity {
    pub(crate) const fn new(index: u32, generation: u32) -> Self {
        Self {
            bits: (generation as u64) << 32 | index as u64,
        }
    }

    pub(crate) fn from_bits(bits: u64) -> Self {
        Self { bits }
    }

    pub fn to_bits(&self) -> u64 {
        self.bits
    }

    pub fn index(&self) -> u32 {
        self.bits as u32
    }

    pub fn generation(&self) -> u32 {
        (self.bits >> 32) as u32
    }
}

#[resource]
pub struct EntityFreeList(pub FreeList);

impl Deref for EntityFreeList {
    type Target = FreeList;
    fn deref(&self) -> &FreeList {
        &self.0
    }
}

impl DerefMut for EntityFreeList {
    fn deref_mut(&mut self) -> &mut FreeList {
        &mut self.0
    }
}

#[resource]
pub struct EntityGenerations(pub DirectMap<u32>);

impl Deref for EntityGenerations {
    type Target = DirectMap<u32>;
    fn deref(&self) -> &DirectMap<u32> {
        &self.0
    }
}

impl DerefMut for EntityGenerations {
    fn deref_mut(&mut self) -> &mut DirectMap<u32> {
        &mut self.0
    }
}

#[param(write)]
pub struct EntityManager {
    free_list: Resource<EntityFreeList>,
    generations: Resource<EntityGenerations>,
    #[read]
    world: WorldAccess,
}

impl<'w> EntityManager<'w, Write> {
    #[inline]
    pub fn spawn(&mut self) -> Entity {
        let free_index = self.free_list.reserve();
        let generation = self
            .generations
            .get(free_index as usize)
            .copied()
            .unwrap_or(0);
        Entity::new(free_index as u32, generation)
    }

    #[inline]
    pub fn despawn(&mut self, entity: Entity) {
        let index = entity.index() as usize;
        let current = self.generations.get(index).copied().unwrap_or(0);
        if current == entity.generation() {
            self.generations.insert(index, current + 1);
            self.free_list.free(index);
        }
    }

    #[inline]
    pub fn is_alive(&self, entity: Entity) -> bool {
        let index = entity.index() as usize;
        self.free_list.bitset.get(index)
            && self.generations.get(index).copied().unwrap_or(0) == entity.generation()
    }

    pub fn components(&self, entity: Entity) -> impl Iterator<Item = ComponentInfo> + '_ {
        let alive = self.is_alive(entity);
        live_components(&self.world, entity).filter(move |_| alive)
    }
}

impl World {
    #[inline]
    pub fn entity_manager(&self) -> Mut<EntityManager<'_>> {
        self.fetch_param::<Mut<EntityManager>>()
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[component]
    struct Health(pub i32);

    #[component]
    struct Armor(pub i32);

    #[component]
    struct Unused(pub i32);

    fn world_with_components() -> World {
        let mut world = World::new();
        world.init_param::<Mut<EntityManager>>();
        world.init_component::<Health>();
        world.init_component::<Armor>();
        world
    }

    #[test]
    fn is_alive_follows_spawn_and_despawn() {
        let mut world = World::new();
        world.init_param::<Mut<EntityManager>>();
        let mut em = world.entity_manager();

        let old = em.spawn();
        assert!(em.is_alive(old));

        em.despawn(old);
        assert!(!em.is_alive(old));

        let new = em.spawn();
        assert!(em.is_alive(new));
        assert!(!em.is_alive(old));
    }

    #[test]
    fn components_lists_what_the_entity_has() {
        let mut world = world_with_components();
        let entity = world.entity_manager().spawn();
        let other = world.entity_manager().spawn();
        world.set_component(entity, Health(1));
        world.set_component(entity, Armor(2));
        world.set_component(other, Armor(3));

        let em = world.entity_manager();
        let uuids: Vec<u128> = em.components(entity).map(|c| c.uuid).collect();
        let mut expected = vec![Health::TYPE_UUID, Armor::TYPE_UUID];
        expected.sort_unstable();
        assert_eq!(uuids, expected);

        let names: Vec<&str> = em.components(other).map(|c| c.type_name).collect();
        assert_eq!(names, vec![std::any::type_name::<Armor>()]);
    }

    #[test]
    fn components_skips_removed_and_uninitialized_components() {
        let mut world = world_with_components();
        let entity = world.entity_manager().spawn();
        world.set_component(entity, Health(1));
        world.remove_component::<Health>(entity);

        assert_eq!(world.entity_manager().components(entity).count(), 0);
        assert!(!world.has_resource::<crate::component::ComponentMap<Unused>>());
    }

    #[test]
    fn components_is_empty_for_despawned_entity() {
        let mut world = world_with_components();
        let entity = world.entity_manager().spawn();
        world.set_component(entity, Health(1));
        world.entity_manager().despawn(entity);

        assert_eq!(world.entity_manager().components(entity).count(), 0);
    }

    #[test]
    fn components_updates_immediately_while_maps_are_held() {
        #[system]
        fn change_and_list(
            mut health: Mut<Component<Health>>,
            mut armor: Mut<Component<Armor>>,
            mut em: Mut<EntityManager>,
        ) {
            let entity = em.spawn();
            assert_eq!(em.components(entity).count(), 0);

            health.insert(entity, Health(1));
            armor.insert(entity, Armor(2));
            assert_eq!(em.components(entity).count(), 2);

            health.remove(entity);
            let names: Vec<&str> = em.components(entity).map(|c| c.type_name).collect();
            assert_eq!(names, vec![std::any::type_name::<Armor>()]);
        }

        let mut world = world_with_components();
        change_and_list.into_system().run(&mut world);
    }

    #[test]
    fn components_can_be_looped_while_changing_components() {
        #[system]
        fn loop_and_change(
            mut health: Mut<Component<Health>>,
            mut armor: Mut<Component<Armor>>,
            mut em: Mut<EntityManager>,
        ) {
            let entity = em.spawn();
            health.insert(entity, Health(1));
            armor.insert(entity, Armor(2));

            for _ in em.components(entity) {
                health.remove(entity);
                armor.remove(entity);
            }
            assert_eq!(em.components(entity).count(), 0);
        }

        let mut world = world_with_components();
        loop_and_change.into_system().run(&mut world);
    }

    #[test]
    fn reused_index_starts_with_no_components_in_mask() {
        let mut world = world_with_components();
        let old = world.entity_manager().spawn();
        world.set_component(old, Health(1));
        world.entity_manager().despawn(old);

        let new = world.entity_manager().spawn();
        assert_eq!(new.index(), old.index());
        assert_eq!(world.entity_manager().components(new).count(), 0);

        world.set_component(new, Armor(1));
        let names: Vec<&str> = world
            .entity_manager()
            .components(new)
            .map(|c| c.type_name)
            .collect();
        assert_eq!(names, vec![std::any::type_name::<Armor>()]);
    }

    #[test]
    fn rejected_insert_from_old_entity_does_not_change_mask() {
        let mut world = world_with_components();
        let old = world.entity_manager().spawn();
        world.entity_manager().despawn(old);
        let new = world.entity_manager().spawn();
        world.set_component(new, Health(1));

        world.set_component(old, Health(2));
        world.set_component(old, Armor(2));

        let uuids: Vec<u128> = world
            .entity_manager()
            .components(new)
            .map(|c| c.uuid)
            .collect();
        assert_eq!(uuids, vec![Health::TYPE_UUID]);
    }

    #[test]
    fn entity_spawns_with_correct_bits() {
        let mut world = World::new();
        world.init_param::<Mut<EntityManager>>();
        let mut em = world.entity_manager();

        for i in 0..25 {
            let entity = em.spawn();
            assert_eq!(entity.index(), i);
        }

        let entity = em.spawn();
        assert_eq!(entity.index(), 25);
        assert_eq!(entity.generation(), 0);

        em.despawn(entity);

        let entity = em.spawn();
        assert_eq!(entity.index(), 25);
        assert_eq!(entity.generation(), 1);
    }
}
