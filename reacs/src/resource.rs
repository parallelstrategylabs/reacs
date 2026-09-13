use std::{any::TypeId, cell::{Ref, RefMut}, ops::{Deref, DerefMut}};

use crate::{Hashed, schedule::{Access, SystemParam}, world::World};

pub struct Resource<'w, T>(Ref<'w, T>);
pub struct ResourceMut<'w, T>(RefMut<'w, T>);

impl<'w, T> Deref for Resource<'w, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'w, T> Deref for ResourceMut<'w, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'w, T> DerefMut for ResourceMut<'w, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: 'static + Default + Hashed> SystemParam for Resource<'_, T> {
    type Item<'w> = Resource<'w, T>;

    fn fetch<'w>(world: &'w World) -> Self::Item<'w> {
        let resource = world
            .get_resource::<T>()
            .expect("Resource not registered in this world!");
        Resource(resource)
    }

    fn access() -> Access {
        Access {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            mutable: false,
        }
    }
}

impl<T: 'static + Default + Hashed> SystemParam for ResourceMut<'_, T> {
    type Item<'w> = ResourceMut<'w, T>;

    fn fetch<'w>(world: &'w World) -> Self::Item<'w> {
        let resource = world
            .get_resource_mut_silent::<T>()
            .expect("Resource not registered in this world!");
        ResourceMut(resource)
    }

    fn access() -> Access {
        Access {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            mutable: true,
        }
    }
}
