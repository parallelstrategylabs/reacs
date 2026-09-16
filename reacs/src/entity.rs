use std::{
    any::TypeId,
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use reacs_derive::{resource, system};

use crate::{
    resource::{self, ResourceMut}, schedule::{Access, SystemParam}, world::World,
};

pub struct Entity {
    bits: u64,
}

impl Entity {
    fn new(index: u32, generation: u32) -> Self {
        Self {
            bits: (generation as u64) << 32 | index as u64,
        }
    }

    fn from_bits(bits: u64) -> Self {
        Self { bits }
    }

    fn to_bits(&self) -> u64 {
        self.bits
    }

    fn index(&self) -> u32 {
        self.bits as u32
    }

    fn generation(&self) -> u32 {
        (self.bits >> 32) as u32
    }
}

pub struct EntityManager {}

impl EntityManager {
    // fn spawn() -> Entity {

    // }
}

impl SystemParam for EntityManager {
    type Item<'w> = EntityManager;

    fn fetch<'w>(world: &'w World) -> Self::Item<'w> {
        EntityManager {}
    }

    fn access() -> Access {
        Access {
            type_id: TypeId::of::<EntityManager>(),
            type_name: std::any::type_name::<EntityManager>(),
            mutable: true,
        }
    }
}