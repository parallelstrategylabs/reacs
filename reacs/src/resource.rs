use std::{
    any::TypeId, cell::{Ref, RefCell, RefMut}, ops::{Deref, DerefMut},
};

use crate::{
    hashed::Hashed,
    schedule::{Access, SystemParam},
    world::World,
};

pub struct Resource<'w, T>(pub Ref<'w, T>);
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

    fn init(world: &mut World) {
        world.init_resource::<T>();
    }

    fn fetch<'w>(world: &'w World) -> Resource<'w, T> {
        Resource(world.get_resource::<T>().unwrap())
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

    fn init(world: &mut World) {
        world.init_resource::<T>();
    }

    fn fetch<'w>(world: &'w World) -> Self::Item<'w> {
        ResourceMut(world.get_resource_mut_silent::<T>().unwrap())
    }

    fn access() -> Access {
        Access {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            mutable: true,
        }
    }
}

// impl World {
//         #[inline]
//     pub fn insert_resource<R: 'static + Hashed>(&mut self, resource: R) {
//         let id = R::TYPE_UUID;
//         if let Some(&index) = self.table.get(&id) {
//             self.storage[index as usize] = Box::new(RefCell::new(resource));
//         } else {
//             self.table.insert(R::TYPE_UUID, self.storage.len() as u32);
//             self.storage.push(Box::new(RefCell::new(resource)));
//         }
//     }

//     #[inline]
//     pub fn init_resource<R: 'static + Default + Hashed>(&mut self) {
//         if !self.has_resource::<R>() {
//             self.insert_resource(R::default());
//         }
//     }

//     #[inline]
//     pub fn remove_resource<R: 'static + Hashed>(&mut self) -> bool {
//         let Some(index) = self.table.remove(&R::TYPE_UUID) else {
//             return false;
//         };
//         self.storage.swap_remove(index as usize);

//         let old_last = self.storage.len() as u32;
//         if index != old_last {
//             for slot in self.table.values_mut() {
//                 if *slot == old_last {
//                     *slot = index;
//                     break;
//                 }
//             }
//         }
//         true
//     }

//     #[inline]
//     pub fn has_resource<R: 'static + Hashed>(&self) -> bool {
//         let id = R::TYPE_UUID;
//         self.table.contains_key(&id)
//     }

//     #[inline]
//     pub fn get_resource<R: 'static + Hashed>(&self) -> Option<Ref<'_, R>> {
//         let id = R::TYPE_UUID;
//         let index = *self.table.get(&id)?;
//         let storage_cell = self.storage.get(index as usize)?;
//         let borrow = storage_cell.borrow();
//         Ref::filter_map(borrow, |any| any.downcast_ref::<R>()).ok()
//     }

//     #[inline]
//     pub fn get_resource_mut<R: 'static + Hashed>(&self) -> Option<RefMut<'_, R>> {
//         self.get_resource_mut_silent::<R>()
//     }

//     #[inline]
//     pub(crate) fn get_resource_mut_silent<R: 'static + Hashed>(&self) -> Option<RefMut<'_, R>> {
//         let id = R::TYPE_UUID;
//         let index = *self.table.get(&id)?;
//         let storage_cell = self.storage.get(index as usize)?;

//         let borrow = storage_cell.borrow_mut();
//         RefMut::filter_map(borrow, |any| any.downcast_mut::<R>()).ok()
//     }
// }

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[resource]
    struct IntResource(pub u64);

    #[test]
    fn system_reads_resource() {
        #[system]
        fn read_system(res: Resource<IntResource>) {
            assert_eq!((*res).0, 94);
        }

        let mut world = World::new();
        world.insert_resource(IntResource(94));

        read_system.into_system().run(&mut world);
    }

    #[test]
    fn system_writes_resource() {
        #[system]
        fn write_system(mut res_mut: ResourceMut<IntResource>) {
            (*res_mut).0 = 234;
        }

        let mut world = World::new();
        write_system.into_system().run(&mut world);

        let res = world.get_resource::<IntResource>();
        assert_eq!((*res.unwrap()).0, 234);
    }
}
