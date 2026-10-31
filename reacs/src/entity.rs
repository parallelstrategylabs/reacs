use std::ops::{Deref, DerefMut};

use reacs_derive::{param, resource};

use crate::{
    direct_map::DirectMap,
    free_list::FreeList,
    resource::Resource,
    schedule::{Mut, Write},
    world::World,
};

pub struct Entity {
    bits: u64,
}

impl Entity {
    pub(crate) fn new(index: u32, generation: u32) -> Self {
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
