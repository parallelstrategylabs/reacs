use reacs_derive::{Hashed, param};
use serde::{Deserialize, Serialize};

use crate::{
    direct_map::DirectMap,
    entity::Entity,
    prelude::*,
    registry::get_registry_entry,
    schedule::{Mode, Read, Write, conflict},
};
use std::{
    any::Any,
    cell::{Ref, RefCell, RefMut},
    marker::PhantomData,
    ops::Deref,
    ptr::addr_of_mut,
};

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
    pub(crate) fn insert(&mut self, entity: Entity, value: C) -> bool {
        let index = entity.index() as usize;
        let generation = entity.generation();
        if self
            .slots
            .get(index)
            .is_some_and(|s| s.generation > generation)
        {
            return false;
        }
        self.slots.insert(index, Slot { generation, value });
        true
    }

    #[inline]
    pub(crate) fn remove(&mut self, entity: Entity) -> bool {
        let found = self.has(entity);
        if found {
            self.slots.remove(entity.index() as usize);
        }
        found
    }
}

#[resource]
pub(crate) struct ComponentMasks {
    #[serde(skip)]
    words: Vec<u64>,
    #[serde(skip)]
    generations: Vec<u32>,
}

impl ComponentMasks {
    fn set(&mut self, entity: Entity, bit: usize, on: bool, row_words: usize) {
        let index = entity.index() as usize;
        if index >= self.generations.len() {
            self.generations.resize(index + 1, 0);
            self.words.resize((index + 1) * row_words, 0);
        }

        let row = index * row_words;
        let generation = entity.generation();
        if self.generations[index] > generation {
            return;
        }
        if self.generations[index] < generation {
            self.words[row..row + row_words].fill(0);
            self.generations[index] = generation;
        }

        let word = &mut self.words[row + bit / 64];
        let mask = 1u64 << (bit % 64);
        if on {
            *word |= mask;
        } else {
            *word &= !mask;
        }
    }

    fn word(&self, entity: Entity, word: usize, row_words: usize) -> u64 {
        let index = entity.index() as usize;
        if self.generations.get(index) != Some(&entity.generation()) {
            return 0;
        }
        self.words[index * row_words + word]
    }
}

fn row_words() -> usize {
    registered_components().len().div_ceil(64)
}

fn component_bit<C: Hashed>() -> usize {
    registered_components()
        .binary_search_by_key(&C::TYPE_UUID, |info| info.uuid)
        .unwrap_or_else(|_| {
            let type_name = std::any::type_name::<C>();
            panic!("Component `{type_name}` not registered, try using #[component] {type_name}!")
        })
}

pub(crate) struct ComponentMaskRef<'w, C, M: Mode = Read> {
    cell: &'w RefCell<dyn Any>,
    bit: usize,
    marker: PhantomData<(C, M)>,
}

impl<C, M: Mode> ComponentMaskRef<'_, C, M> {
    #[inline]
    fn set(&self, entity: Entity, on: bool) {
        self.cell
            .borrow_mut()
            .downcast_mut::<ComponentMasks>()
            .expect("component mask resource has the wrong type")
            .set(entity, self.bit, on, row_words());
    }
}

impl<C: 'static + Hashed, M: Mode> SystemParam for ComponentMaskRef<'_, C, M> {
    type Item<'w> = ComponentMaskRef<'w, C, M>;

    fn init(world: &mut World) {
        world.init_resource::<ComponentMasks>();
    }

    fn fetch<'w>(world: &'w World) -> ComponentMaskRef<'w, C, M> {
        let i = world
            .resources
            .binary_search_by_key(&ComponentMasks::TYPE_UUID, |resource| resource.0)
            .unwrap_or_else(|_| conflict::<ComponentMasks>());
        ComponentMaskRef {
            cell: &*world.resources[i].1,
            bit: component_bit::<C>(),
            marker: PhantomData,
        }
    }
}

#[param]
pub struct Component<C: Default + Copy + Hashed> {
    map: Resource<ComponentMap<C>>,
    #[read]
    masks: ComponentMaskRef<C>,
}

impl<C: 'static + Default + Copy + Hashed, M: Mode> Deref for Component<'_, C, M> {
    type Target = ComponentMap<C>;

    fn deref(&self) -> &ComponentMap<C> {
        &self.map
    }
}

impl<C: 'static + Default + Copy + Hashed> Component<'_, C, Write> {
    #[inline]
    pub fn get_mut(&mut self, entity: Entity) -> Option<&mut C> {
        self.map.get_mut(entity)
    }

    #[inline]
    pub fn insert(&mut self, entity: Entity, value: C) {
        if self.map.insert(entity, value) {
            self.masks.set(entity, true);
        }
    }

    #[inline]
    pub fn remove(&mut self, entity: Entity) {
        if self.map.remove(entity) {
            self.masks.set(entity, false);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentInfo {
    pub uuid: u128,
    pub type_name: &'static str,
}

static mut COMPONENT_REGISTRY: Vec<ComponentInfo> = Vec::new();

pub fn register_component<C: 'static + Copy + Default + Hashed>() {
    let info = ComponentInfo {
        uuid: C::TYPE_UUID,
        type_name: std::any::type_name::<C>(),
    };
    unsafe {
        let registry = &mut *addr_of_mut!(COMPONENT_REGISTRY);
        let i = registry
            .binary_search_by_key(&info.uuid, |e| e.uuid)
            .unwrap_or_else(|i| i);
        registry.insert(i, info);
    }
}

fn registered_components() -> &'static [ComponentInfo] {
    unsafe { &*addr_of_mut!(COMPONENT_REGISTRY) }
}

pub(crate) fn live_components(
    world: &World,
    entity: Entity,
) -> impl Iterator<Item = ComponentInfo> + '_ {
    let row_words = row_words();
    (0..row_words).flat_map(move |w| {
        let mut word = world
            .get_resource::<ComponentMasks>()
            .map_or(0, |masks| masks.word(entity, w, row_words));
        std::iter::from_fn(move || {
            if word == 0 {
                return None;
            }
            let bit = word.trailing_zeros() as usize;
            word &= word - 1;
            Some(registered_components()[w * 64 + bit])
        })
    })
}

impl World {
    #[inline]
    pub fn init_component<C: 'static + Copy + Default + Hashed>(&mut self) {
        let id = ComponentMap::<C>::TYPE_UUID;
        get_registry_entry(id).unwrap_or_else(|| {
            let type_name = std::any::type_name::<C>();
            panic!("Component `{type_name}` not registered, try using #[component] {type_name}!")
        });

        if !self.has_resource::<ComponentMap<C>>() {
            self.insert_resource(ComponentMap::<C>::new());
        }
        self.init_resource::<ComponentMasks>();
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
    pub fn get_component_map_mut<C: 'static + Copy + Default + Hashed>(
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
    pub fn set_component<C: 'static + Copy + Default + Hashed>(
        &mut self,
        entity: Entity,
        value: C,
    ) {
        if self.get_component_map_mut::<C>().insert(entity, value) {
            self.set_component_mask::<C>(entity, true);
        }
    }

    #[inline]
    pub fn remove_component<C: 'static + Copy + Default + Hashed>(&mut self, entity: Entity) {
        if self.get_component_map_mut::<C>().remove(entity) {
            self.set_component_mask::<C>(entity, false);
        }
    }

    fn set_component_mask<C: Hashed>(&self, entity: Entity, on: bool) {
        self.get_resource_mut::<ComponentMasks>()
            .unwrap_or_else(|| conflict::<ComponentMasks>())
            .set(entity, component_bit::<C>(), on, row_words());
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
    use super::ComponentMasks;
    use crate::{entity::Entity, prelude::*, schedule::Mut};

    #[component]
    struct IntComponent(pub i32);

    #[component]
    struct OtherComponent(pub i32);

    #[signal]
    struct Hit(pub Entity);

    #[test]
    fn component_mut_inserts() {
        const KEY: Entity = Entity::new(18, 0);

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
        const KEY: Entity = Entity::new(7, 0);

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
        let entity = Entity::new(3, 0);
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
    fn mask_handles_bits_past_the_first_word() {
        let entity = Entity::new(2, 0);
        let mut masks = ComponentMasks::default();

        masks.set(entity, 70, true, 2);
        assert_eq!(masks.word(entity, 0, 2), 0);
        assert_eq!(masks.word(entity, 1, 2), 1 << 6);

        masks.set(entity, 70, false, 2);
        assert_eq!(masks.word(entity, 1, 2), 0);
    }

    #[test]
    fn mask_row_resets_for_newer_generation_and_ignores_older() {
        let old = Entity::new(0, 0);
        let new = Entity::new(0, 1);
        let mut masks = ComponentMasks::default();

        masks.set(old, 1, true, 1);
        assert_eq!(masks.word(new, 0, 1), 0);

        masks.set(new, 2, true, 1);
        assert_eq!(masks.word(new, 0, 1), 1 << 2);
        assert_eq!(masks.word(old, 0, 1), 0);

        masks.set(old, 3, true, 1);
        assert_eq!(masks.word(new, 0, 1), 1 << 2);
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
