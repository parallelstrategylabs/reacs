//! Entities, their handles, and the param that spawns and despawns them.

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

/// A handle to an entity: an index plus a generation.
///
/// An entity is only an id; its data lives in components. Spawning takes the lowest free index,
/// and despawning frees it and moves its generation on, so a handle kept after despawn never
/// matches the entity that reuses its index.
///
/// Handles are plain data: store them in components, signals and resources.
/// `Entity::default()` is never alive.
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

    /// Returns the handle as one number: the generation in the high 32 bits, the index in the
    /// low 32 bits.
    pub fn to_bits(&self) -> u64 {
        self.bits
    }

    /// Returns the index, the entity's slot in every component's storage.
    ///
    /// Indexes are reused: after a despawn, the next spawn may get the same index with a newer
    /// generation.
    pub fn index(&self) -> u32 {
        self.bits as u32
    }

    /// Returns the generation, which tells apart entities that used the same index.
    pub fn generation(&self) -> u32 {
        (self.bits >> 32) as u32
    }
}

#[resource]
pub(crate) struct EntityFreeList(pub FreeList);

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
pub(crate) const NO_GENERATION: u32 = u32::MAX;

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

/// System param that spawns, despawns and checks entities.
///
/// `EntityManager` reads: [`is_alive`](Self::is_alive) and [`generation`](Self::generation).
/// `Mut<EntityManager>` also [spawns](Self::spawn) and [despawns](Self::despawn). The host reads
/// it with [`World::entities`].
///
/// # Examples
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[signal(input)]
/// struct SpawnWave {
///     size: u32,
/// }
///
/// #[resource]
/// struct Army {
///     units: Vec<Entity>,
/// }
///
/// #[system]
/// fn spawn_wave(
///     mut waves: Mut<Receiver<SpawnWave>>,
///     mut entities: Mut<EntityManager>,
///     mut army: Mut<Resource<Army>>,
/// ) {
///     for wave in waves.receive() {
///         for _ in 0..wave.size {
///             army.units.push(entities.spawn());
///         }
///     }
/// }
///
/// fn main() {
///     let mut world = World::new();
///     world.input(SpawnWave { size: 3 });
///     world.step();
///
///     let army = world.get_resource::<Army>().unwrap();
///     assert_eq!(army.units.len(), 3);
///     assert!(army.units.iter().all(|&unit| world.entities().is_alive(unit)));
/// }
/// ```
#[param]
pub struct EntityManager {
    free_list: Resource<EntityFreeList>,
    #[read]
    generations: ResourceCell<EntityGenerations>,
}

impl<M: Mode> EntityManager<'_, M> {
    /// Returns the generation of the entity at `index`, or the one the next entity spawned there
    /// will get if the index is free.
    #[inline]
    pub fn generation(&self, index: u32) -> u32 {
        self.generations.read().current(index as usize)
    }

    /// Returns `true` if `entity` was spawned and hasn't been despawned.
    #[inline]
    pub fn is_alive(&self, entity: Entity) -> bool {
        self.free_list.bitset.get(entity.index() as usize)
            && self.generation(entity.index()) == entity.generation()
    }
}

/// What [`AliveFilter`] reads to tell whether an entity is alive.
///
/// It compares generations only, which is right for any handle [`EntityManager::spawn`] returned:
/// despawning raises the generation. It never borrows the entity manager's free list, so it can't
/// conflict with a `Mut<EntityManager>` in the same system.
pub struct EntityLife<'w> {
    generations: ResourceCell<'w, EntityGenerations>,
}

impl EntityLife<'_> {
    /// Returns `true` if `entity` hasn't been despawned since it was spawned.
    #[inline]
    pub fn is_alive(&self, entity: Entity) -> bool {
        self.generations.read().current(entity.index() as usize) == entity.generation()
    }
}

/// A signal tied to the life of one entity. `#[signal(entity)]` implements it.
pub trait EntitySignal {
    /// Returns the entity the signal is about.
    fn entity(&self) -> Entity;
}

/// The filter of `#[signal(entity)]` types: a signal stops being delivered once its entity is
/// despawned, and an open-ended one is then dropped from memory.
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
    /// Spawns an entity and returns its handle.
    ///
    /// The entity takes the lowest free index, so the same spawns give the same handles on every
    /// run. It has no components until they're inserted.
    #[inline]
    pub fn spawn(&mut self) -> Entity {
        let free_index = self.free_list.reserve();
        let generation = self.generations.read().current(free_index as usize);
        Entity::new(free_index as u32, generation)
    }

    /// Despawns `entity` immediately. Does nothing if it's already despawned.
    ///
    /// Signals of `#[signal(entity)]` types naming it stop being delivered at once, even later in
    /// a receive loop that is running. Its components aren't touched: their values stay as dead
    /// data until an entity that reuses the index writes over them, and that entity can't see
    /// them. A copy of the handle can still read them until then, so read what you need before
    /// despawning, not after.
    ///
    /// # Limits
    ///
    /// Each despawn adds one to the index's generation, so after 2<sup>31</sup> despawns of one
    /// index since a component was written there, inserting that component for the entity now at
    /// that index is silently ignored. Spawning and despawning one throwaway entity after another
    /// reuses the same index every time; spawn them in batches to reduce the risk.
    #[inline]
    pub fn despawn(&mut self, entity: Entity) {
        let index = entity.index() as usize;
        let current = self.generations.read().current(index);
        if current == entity.generation() {
            let next = match current + 1 {
                NO_GENERATION => FIRST_GENERATION,
                next => next,
            };
            self.generations.write().insert(index, next);
            self.free_list.free(index);
        }
    }
}

impl World {
    #[inline]
    pub(crate) fn entity_manager(&self) -> Mut<EntityManager<'_>> {
        self.fetch_param::<Mut<EntityManager>>()
    }

    /// Returns the entity manager for reading, to check whether entities are alive.
    #[inline]
    pub fn entities(&self) -> EntityManager<'_> {
        self.fetch_param::<EntityManager>()
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
    fn despawn_skips_the_empty_generation() {
        let mut world = world_with_components();
        world
            .get_resource_mut::<super::EntityGenerations>()
            .unwrap()
            .insert(0, super::NO_GENERATION - 1);
        let last = world.entity_manager().spawn();
        assert_eq!(last.generation(), super::NO_GENERATION - 1);
        world.set_component(last, Health(1));

        world.entity_manager().despawn(last);
        let next = world.entity_manager().spawn();

        assert_eq!(next.index(), last.index());
        assert_eq!(next.generation(), super::FIRST_GENERATION);
        assert!(!world.has_component::<Health>(next));
        world.set_component(next, Health(2));
        assert_eq!(world.get_component::<Health>(next).unwrap().0, 2);
    }

    #[test]
    fn default_entity_is_never_alive() {
        let world = world_with_components();
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
