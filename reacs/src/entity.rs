use std::any::TypeId;

use reacs_derive::resource;

use crate::{
    direct_map::DirectMap,
    free_list::FreeList,
    resource::ResourceMut,
    schedule::{Access, SystemParam},
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

#[resource]
pub struct EntityGenerations(pub DirectMap<u32>);

pub struct EntityManager<'w> {
    pub free_list: ResourceMut<'w, EntityFreeList>,
    pub generations: ResourceMut<'w, EntityGenerations>,
}

impl<'w> EntityManager<'w> {
    #[inline]
    pub fn spawn(&mut self) -> Entity {
        let free_index = self.free_list.0.reserve();
        let generation = self
            .generations
            .0
            .get(free_index as usize)
            .copied()
            .unwrap_or(0u32);

        Entity::new(free_index as u32, generation)
    }

    #[inline]
    pub fn despawn(&mut self, entity: Entity) {
        let index = entity.index() as usize;
        let current_generation = self.generations.0.get(index).copied().unwrap_or(0u32);
        if current_generation == entity.generation() {
            self.generations.0.insert(index, current_generation + 1);
            self.free_list.0.free(index);
        }
    }
}

impl SystemParam for EntityManager<'_> {
    type Item<'w> = EntityManager<'w>;

    fn init(world: &mut World) {
        ResourceMut::<EntityFreeList>::init(world);
        ResourceMut::<EntityGenerations>::init(world);
    }

    fn fetch<'w>(world: &'w World) -> Self::Item<'w> {
        EntityManager {
            free_list: ResourceMut::fetch(world),
            generations: ResourceMut::fetch(world),
        }
    }

    fn access() -> Access {
        Access {
            type_id: TypeId::of::<EntityManager>(),
            type_name: std::any::type_name::<EntityManager>(),
            mutable: true,
        }
    }
}

impl World {
    #[inline]
    pub fn entity_manager(&self) -> EntityManager<'_> {
        self.fetch_param::<EntityManager>()
    }
}

#[cfg(test)]
mod tests {
    use crate::{entity::EntityManager, world::World};

    #[test]
    fn entity_spawns_with_correct_bits() {
        let mut world = World::new();
        world.init_param::<EntityManager>();

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
