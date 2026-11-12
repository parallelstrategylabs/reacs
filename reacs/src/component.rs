use reacs_derive::Hashed;
use serde::{Deserialize, Serialize};

use crate::{
    entity::{Entity, NO_GENERATION},
    prelude::*,
    registry::get_registry_entry,
    schedule::{Mode, ParamInit, ParamWorld, Read, SystemParam, WithMode, Write},
};
use std::cell::{Ref, RefMut};

const EMPTY: u32 = NO_GENERATION;

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Slot<C> {
    generation: u32,
    value: C,
}

#[derive(Default, Hashed, Serialize, Deserialize)]
pub struct ComponentMap<C: Copy + Default> {
    slots: Vec<Slot<C>>,
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
        let generation = entity.generation();
        self.slots
            .get(entity.index() as usize)
            .filter(|slot| slot.generation == generation)
            .map(|slot| &slot.value)
    }

    #[inline]
    pub fn get_mut(&mut self, entity: Entity) -> Option<&mut C> {
        let generation = entity.generation();
        self.slots
            .get_mut(entity.index() as usize)
            .filter(|slot| slot.generation == generation)
            .map(|slot| &mut slot.value)
    }

    #[inline]
    pub(crate) fn insert(&mut self, entity: Entity, value: C) {
        let index = entity.index() as usize;
        let generation = entity.generation();
        if generation == EMPTY || generation == 0 {
            return;
        }
        if index >= self.slots.len() {
            let new_size = (index + 1).next_power_of_two();
            let mut slots = Vec::with_capacity(new_size);
            slots.extend_from_slice(&self.slots);
            let empty = Slot {
                generation: EMPTY,
                value: C::default(),
            };
            slots.resize(new_size, empty);
            self.slots = slots;
        }
        let slot = &mut self.slots[index];
        if slot.generation != EMPTY && slot.generation.wrapping_sub(generation) as i32 > 0 {
            return;
        }
        *slot = Slot { generation, value };
    }

    #[inline]
    pub(crate) fn remove(&mut self, entity: Entity) {
        let generation = entity.generation();
        if let Some(slot) = self.slots.get_mut(entity.index() as usize) {
            if slot.generation == generation {
                slot.generation = EMPTY;
            }
        }
    }
}

pub struct Component<'w, C: 'static + Copy + Default, M: Mode = Read> {
    slots: *mut Slot<C>,
    len: usize,
    map: M::Guard<'w, ComponentMap<C>>,
}

impl<'w, C: 'static + Copy + Default, M0: Mode, M: Mode> WithMode<M> for Component<'w, C, M0> {
    type Out = Component<'w, C, M>;
}

impl<C: 'static + Copy + Default, M: Mode> Component<'_, C, M> {
    #[inline]
    fn slot(&self, entity: Entity) -> Option<*mut Slot<C>> {
        let index = entity.index() as usize;
        let generation = entity.generation();
        if index >= self.len {
            return None;
        }
        let slot = unsafe { self.slots.add(index) };
        (unsafe { (*slot).generation } == generation).then_some(slot)
    }

    #[inline]
    pub fn has(&self, entity: Entity) -> bool {
        self.slot(entity).is_some()
    }

    #[inline]
    pub fn get(&self, entity: Entity) -> Option<C> {
        self.slot(entity).map(|slot| unsafe { (*slot).value })
    }
}

impl<C: 'static + Copy + Default> Component<'_, C, Write> {
    #[inline]
    pub fn get_mut(&mut self, entity: Entity) -> Option<&mut C> {
        self.slot(entity).map(|slot| unsafe { &mut (*slot).value })
    }

    #[inline]
    pub fn insert(&mut self, entity: Entity, value: C) {
        self.map.insert(entity, value);
        self.slots = self.map.slots.as_mut_ptr();
        self.len = self.map.slots.len();
    }

    #[inline]
    pub fn remove(&mut self, entity: Entity) {
        if let Some(slot) = self.slot(entity) {
            unsafe { (*slot).generation = EMPTY };
        }
    }
}

impl<C: 'static + Copy + Default + Hashed, M: Mode> SystemParam for Component<'_, C, M> {
    type Item<'w> = Component<'w, C, M>;

    fn init(world: ParamInit<'_>) {
        world.0.init_resource::<ComponentMap<C>>();
    }

    fn fetch<'w>(world: ParamWorld<'w>) -> Component<'w, C, M> {
        let mut map = M::fetch::<ComponentMap<C>>(world);
        let slots = match M::guard_mut(&mut map) {
            Some(map) => map.slots.as_mut_ptr(),
            None => map.slots.as_ptr().cast_mut(),
        };
        let len = map.slots.len();
        Component { slots, len, map }
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

    #[test]
    fn default_entity_never_has_components() {
        let entity = Entity::new(0, 1);
        let mut world = World::new();
        world.init_component::<IntComponent>();
        world.set_component(Entity::new(1, 1), IntComponent(1));
        world.set_component(Entity::default(), IntComponent(2));

        assert!(!world.has_component::<IntComponent>(Entity::default()));
        assert!(!world.has_component::<IntComponent>(entity));

        world.set_component(entity, IntComponent(3));
        world.remove_component::<IntComponent>(entity);
        assert!(!world.has_component::<IntComponent>(Entity::default()));
        assert!(!world.has_component::<IntComponent>(entity));
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

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    fn next_generation(generation: u32) -> u32 {
        if generation + 1 == u32::MAX {
            1
        } else {
            generation + 1
        }
    }

    #[derive(Default)]
    struct Model {
        generations: Vec<u32>,
        alive: Vec<bool>,
        alive_count: usize,
        free: std::collections::BTreeSet<usize>,
        owned: Vec<Option<i32>>,
        slots: Vec<Option<(u32, i32)>>,
    }

    impl Model {
        fn spawn(&mut self) -> Entity {
            let index = match self.free.pop_first() {
                Some(index) => index,
                None => {
                    self.generations.push(1);
                    self.alive.push(false);
                    self.owned.push(None);
                    self.alive.len() - 1
                }
            };
            self.alive[index] = true;
            self.alive_count += 1;
            self.owned[index] = None;
            Entity::new(index as u32, self.generations[index])
        }

        fn is_current(&self, entity: Entity) -> bool {
            let index = entity.index() as usize;
            index < self.alive.len()
                && self.alive[index]
                && self.generations[index] == entity.generation()
        }

        fn despawn(&mut self, entity: Entity) {
            let index = entity.index() as usize;
            if self.generations[index] == entity.generation() {
                self.generations[index] = next_generation(entity.generation());
                if self.alive[index] {
                    self.alive_count -= 1;
                    self.free.insert(index);
                }
                self.alive[index] = false;
            }
        }

        fn slot(&self, entity: Entity) -> Option<i32> {
            match self.slots.get(entity.index() as usize) {
                Some(Some((generation, value))) if *generation == entity.generation() => {
                    Some(*value)
                }
                _ => None,
            }
        }

        fn insert(&mut self, entity: Entity, value: i32) {
            let (index, generation) = (entity.index() as usize, entity.generation());
            if self.is_current(entity) {
                self.owned[index] = Some(value);
            }
            if generation == 0 || generation == u32::MAX {
                return;
            }
            if index >= self.slots.len() {
                self.slots.resize(index + 1, None);
            }
            if let Some((newest, _)) = self.slots[index] {
                if newest.wrapping_sub(generation) as i32 > 0 {
                    return;
                }
            }
            self.slots[index] = Some((generation, value));
        }

        fn remove(&mut self, entity: Entity) {
            let index = entity.index() as usize;
            if self.is_current(entity) {
                self.owned[index] = None;
            }
            if self.slot(entity).is_some() {
                self.slots[index] = None;
            }
        }

        fn increment(&mut self, entity: Entity) {
            let index = entity.index() as usize;
            if self.is_current(entity) {
                if let Some(value) = &mut self.owned[index] {
                    *value += 1;
                }
            }
            if self.slot(entity).is_some() {
                if let Some((_, value)) = &mut self.slots[index] {
                    *value += 1;
                }
            }
        }
    }

    fn model_world() -> World {
        let mut world = World::new();
        world.init_param::<Mut<EntityManager>>();
        world.init_component::<IntComponent>();
        world
    }

    fn check_everything(world: &World, model: &Model, handles: &[Entity]) {
        let em = world.entities();
        let components = world.fetch_param::<Component<IntComponent>>();
        let map = world.get_component_map::<IntComponent>();
        for (index, &generation) in model.generations.iter().enumerate() {
            assert_eq!(em.generation(index as u32), generation, "index {index}");
            let current = Entity::new(index as u32, generation);
            assert_eq!(em.is_alive(current), model.alive[index], "{current:?}");
            if model.alive[index] {
                assert_eq!(components.get(current).map(|c| c.0), model.owned[index]);
            }
        }
        for &handle in handles {
            assert_eq!(em.is_alive(handle), model.is_current(handle), "{handle:?}");
            assert_eq!(components.get(handle).map(|c| c.0), model.slot(handle));
            assert_eq!(map.get(handle).map(|c| c.0), model.slot(handle));
            assert_eq!(components.has(handle), model.slot(handle).is_some());
        }
    }

    #[test]
    fn entities_and_components_match_a_simple_model() {
        let steps = crate::test_size(40_000, 1_500);
        let burst = crate::test_size(64, 8) as u64;
        let target = crate::test_size(10_000, 300);
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let mut world = model_world();
        let mut model = Model::default();
        let mut handles: Vec<Entity> = Vec::new();
        let mut most_alive = 0;
        let mut wraps = 0;
        let mut loads = 0;

        for step in 0..steps {
            let growing = model.alive_count
                < if (step / 4_000) % 2 == 0 {
                    target
                } else {
                    target / 20
                };
            let roll = rng.below(1000);
            if roll < if growing { 350 } else { 200 } {
                let mut em = world.entity_manager();
                for _ in 0..1 + rng.below(burst) {
                    let entity = em.spawn();
                    assert_eq!(entity, model.spawn(), "step {step}");
                    handles.push(entity);
                }
            } else if roll < 550 {
                let mut em = world.entity_manager();
                let count = if growing { 1 } else { 1 + rng.below(burst) };
                for _ in 0..count {
                    if handles.is_empty() {
                        break;
                    }
                    let index = rng.below(model.alive.len().max(1) as u64) as usize;
                    let entity = if rng.below(4) != 0 && model.alive.get(index) == Some(&true) {
                        Entity::new(index as u32, model.generations[index])
                    } else {
                        let at = rng.below(handles.len() as u64) as usize;
                        let entity = handles[at];
                        if !model.is_current(entity) && rng.below(2) == 0 {
                            handles.swap_remove(at);
                        }
                        entity
                    };
                    em.despawn(entity);
                    model.despawn(entity);
                    assert!(!em.is_alive(entity));
                }
            } else if roll < 990 {
                let mut components = world.fetch_param::<Mut<Component<IntComponent>>>();
                for _ in 0..1 + rng.below(8) {
                    let len = model.alive.len() as u64;
                    let pick = rng.below(20);
                    let entity = match pick {
                        0 => Entity::default(),
                        1 => Entity::new(rng.below(len + 100) as u32, 0),
                        2 => Entity::new(rng.below(len + 100) as u32, u32::MAX),
                        3 => Entity::new((len + rng.below(5_000)) as u32, 1),
                        4..=9 if !handles.is_empty() => {
                            handles[rng.below(handles.len() as u64) as usize]
                        }
                        _ => {
                            let index = rng.below(len.max(1)) as usize;
                            match model.alive.get(index) {
                                Some(true) => Entity::new(index as u32, model.generations[index]),
                                _ => Entity::default(),
                            }
                        }
                    };
                    let readable = pick != 2;
                    let writable = pick != 3;
                    match rng.below(10) {
                        0..=3 if writable => {
                            let value = rng.below(1_000_000) as i32;
                            components.insert(entity, IntComponent(value));
                            model.insert(entity, value);
                        }
                        4 => {
                            components.remove(entity);
                            model.remove(entity);
                        }
                        5 | 6 if readable => {
                            if let Some(c) = components.get_mut(entity) {
                                c.0 += 1;
                            }
                            model.increment(entity);
                        }
                        _ if readable => {
                            assert_eq!(components.has(entity), model.slot(entity).is_some());
                        }
                        _ => {}
                    }
                    if readable {
                        assert_eq!(
                            components.get(entity).map(|c| c.0),
                            model.slot(entity),
                            "step {step}, {entity:?}"
                        );
                        if model.is_current(entity) {
                            assert_eq!(
                                components.get(entity).map(|c| c.0),
                                model.owned[entity.index() as usize]
                            );
                        }
                    }
                }
            } else if roll < 997 {
                let empty: Vec<usize> = model
                    .free
                    .iter()
                    .copied()
                    .filter(|&index| model.slots.get(index).is_none_or(|slot| slot.is_none()))
                    .collect();
                if !empty.is_empty() {
                    let index = empty[rng.below(empty.len() as u64) as usize];
                    let generation = u32::MAX - 1 - rng.below(3) as u32;
                    world
                        .get_resource_mut::<crate::entity::EntityGenerations>()
                        .unwrap()
                        .insert(index, generation);
                    model.generations[index] = generation;
                    handles.retain(|handle| handle.index() as usize != index);
                    wraps += 1;
                }
            } else {
                let bytes = world.save();
                world = model_world();
                world.load(&bytes);
                loads += 1;
            }

            while handles.len() > target * 2 {
                let at = rng.below(handles.len() as u64) as usize;
                handles.swap_remove(at);
            }
            most_alive = most_alive.max(model.alive_count);
            if step % 64 == 0 {
                check_everything(&world, &model, &handles);
            }
        }
        check_everything(&world, &model, &handles);

        let wrapped = model.generations.iter().filter(|g| **g < 10).count();
        println!("most alive {most_alive}, generation jumps {wraps}, loads {loads}");
        assert!(wraps > 3 && loads > 3);
        assert!(model.generations.iter().any(|g| *g > 1) && wrapped > 0);
        if !cfg!(miri) {
            assert!(most_alive > 4_096 * 2, "{most_alive}");
        }
    }
}
