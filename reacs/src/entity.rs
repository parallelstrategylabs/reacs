use std::ops::{Deref, DerefMut};

use reacs_derive::{param, resource};
use serde::{Deserialize, Serialize};

use crate::{
    component::{ComponentInfo, ComponentMasks, live_components, row_words},
    direct_map::DirectMap,
    free_list::FreeList,
    resource::{Resource, ResourceCell},
    schedule::{Mode, Mut, Write},
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

const FIRST_GENERATION: u32 = 1;

#[resource]
pub(crate) struct EntityGenerations(pub DirectMap<u32>);

impl EntityGenerations {
    #[inline]
    pub(crate) fn current(&self, index: usize) -> u32 {
        self.get(index).copied().unwrap_or(FIRST_GENERATION)
    }
}

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

#[param]
pub struct EntityManager {
    free_list: Resource<EntityFreeList>,
    #[read]
    generations: ResourceCell<EntityGenerations>,
    #[read]
    masks: ResourceCell<ComponentMasks>,
}

impl<M: Mode> EntityManager<'_, M> {
    #[inline]
    pub fn generation(&self, index: u32) -> u32 {
        self.generations.read().current(index as usize)
    }

    #[inline]
    pub fn is_alive(&self, entity: Entity) -> bool {
        self.free_list.bitset.get(entity.index() as usize)
            && self.generation(entity.index()) == entity.generation()
    }

    pub fn components(&self, entity: Entity) -> impl Iterator<Item = ComponentInfo> + '_ {
        live_components(&self.masks, entity, self.is_alive(entity))
    }
}

impl EntityManager<'_, Write> {
    #[inline]
    pub fn spawn(&mut self) -> Entity {
        let free_index = self.free_list.reserve();
        let generation = self.generations.read().current(free_index as usize);
        Entity::new(free_index as u32, generation)
    }

    #[inline]
    pub fn despawn(&mut self, entity: Entity) {
        let index = entity.index() as usize;
        let current = self.generations.read().current(index);
        if current == entity.generation() {
            self.generations.write().insert(index, current + 1);
            self.free_list.free(index);
            self.masks.write().clear(entity, row_words());
        }
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
    fn read_only_entity_manager_checks_entities() {
        const ALIVE: Entity = Entity::new(0, 1);
        const DEAD: Entity = Entity::new(1, 1);

        #[system]
        fn read_entities(em: EntityManager, health: Component<Health>) {
            assert!(em.is_alive(ALIVE));
            assert!(health.has(ALIVE));
            assert_eq!(em.components(ALIVE).count(), 1);
            assert_eq!(em.generation(ALIVE.index()), 1);

            assert!(!em.is_alive(DEAD));
            assert_eq!(em.generation(DEAD.index()), 2);
            assert_eq!(em.components(DEAD).count(), 0);
        }

        let mut world = world_with_components();
        let alive = world.entity_manager().spawn();
        let dead = world.entity_manager().spawn();
        assert_eq!((alive, dead), (ALIVE, DEAD));
        world.set_component(alive, Health(1));
        world.entity_manager().despawn(dead);

        read_entities.into_system().run(&mut world);
    }

    #[test]
    fn read_only_entity_manager_works_inside_custom_param() {
        #[param]
        struct AliveHealth {
            health: Component<Health>,
            #[read]
            entities: EntityManager,
        }

        #[system]
        fn read_view(view: AliveHealth) {
            assert!(!view.entities.is_alive(Entity::new(5, 1)));
            assert!(!view.health.has(Entity::new(5, 1)));
        }

        let mut world = world_with_components();
        read_view.into_system().run(&mut world);
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn read_and_write_entity_manager_in_one_system_panics() {
        #[system]
        fn both(_read: EntityManager, _write: Mut<EntityManager>) {}

        let mut world = world_with_components();
        both.into_system().run(&mut world);
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
        assert_eq!(entity.generation(), 1);

        em.despawn(entity);

        let entity = em.spawn();
        assert_eq!(entity.index(), 25);
        assert_eq!(entity.generation(), 2);
    }

    #[test]
    fn default_entity_is_never_alive() {
        let mut world = world_with_components();
        let default = Entity::default();
        assert!(!world.entity_manager().is_alive(default));

        let first = world.entity_manager().spawn();

        assert_eq!(first.index(), default.index());
        assert_ne!(first, default);
        assert!(!world.entity_manager().is_alive(default));
    }

    #[test]
    fn component_on_default_entity_does_not_reach_spawned_entity() {
        let mut world = world_with_components();
        world.set_component(Entity::default(), Health(1));

        let first = world.entity_manager().spawn();

        assert!(!world.has_component::<Health>(first));
        assert_eq!(world.entity_manager().components(first).count(), 0);
    }

    #[test]
    fn default_entity_cannot_change_live_entity() {
        let mut world = world_with_components();
        let first = world.entity_manager().spawn();
        world.set_component(first, Health(1));
        let default = Entity::default();

        assert!(!world.has_component::<Health>(default));
        world.set_component(default, Health(2));
        world.remove_component::<Health>(default);
        world.entity_manager().despawn(default);

        assert!(world.entity_manager().is_alive(first));
        assert_eq!(world.get_component::<Health>(first).unwrap().0, 1);
        assert_eq!(world.entity_manager().components(first).count(), 1);
    }
}
