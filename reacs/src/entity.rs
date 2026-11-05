use std::ops::{Deref, DerefMut};

use reacs_derive::{param, resource};
use serde::{Deserialize, Serialize};

use crate::{
    direct_map::DirectMap,
    free_list::FreeList,
    resource::{Resource, ResourceCell},
    schedule::{Mode, Mut, Write},
    signal::SignalFilter,
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
}

pub struct EntityLife<'w> {
    generations: ResourceCell<'w, EntityGenerations>,
}

impl EntityLife<'_> {
    #[inline]
    pub fn is_alive(&self, entity: Entity) -> bool {
        self.generations.read().current(entity.index() as usize) == entity.generation()
    }
}

pub trait EntitySignal {
    fn entity(&self) -> Entity;
}

pub struct AliveFilter;

impl<S: EntitySignal> SignalFilter<S> for AliveFilter {
    const ACTIVE: bool = true;
    type Context<'w> = EntityLife<'w>;

    #[inline]
    fn fetch<'w>(world: &'w World) -> EntityLife<'w> {
        EntityLife {
            generations: world.fetch_param::<ResourceCell<EntityGenerations>>(),
        }
    }

    #[inline]
    fn keep(context: &EntityLife<'_>, signal: &S) -> bool {
        context.is_alive(signal.entity())
    }

    #[inline]
    fn stamp(world: &World) -> u64 {
        world
            .get_resource::<EntityFreeList>()
            .map_or(0, |free_list| free_list.frees)
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

    fn world_with_components() -> World {
        let mut world = World::new();
        world.init_param::<Mut<EntityManager>>();
        world.init_component::<Health>();
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
            assert_eq!(em.generation(ALIVE.index()), 1);

            assert!(!em.is_alive(DEAD));
            assert_eq!(em.generation(DEAD.index()), 2);
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
    }
}
