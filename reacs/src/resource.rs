use crate::{
    hashed::Hashed,
    schedule::{Mode, Read, SystemParam, WithMode, conflict},
    world::World,
};
use std::{
    any::Any,
    cell::{Ref, RefCell, RefMut},
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

pub struct Resource<'w, T: 'static, M: Mode = Read>(pub M::Guard<'w, T>);

impl<'w, T: 'static, M0: Mode, M: Mode> WithMode<M> for Resource<'w, T, M0> {
    type Out = Resource<'w, T, M>;
}

impl<'w, T: 'static, M: Mode> Deref for Resource<'w, T, M> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<'w, T: 'static, M: Mode> DerefMut for Resource<'w, T, M>
where
    M::Guard<'w, T>: DerefMut,
{
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: 'static + Default + Hashed, M: Mode> SystemParam for Resource<'_, T, M> {
    type Item<'w> = Resource<'w, T, M>;

    fn init(world: &mut World) {
        world.init_resource::<T>();
    }

    fn fetch<'w>(world: &'w World) -> Resource<'w, T, M> {
        Resource(M::fetch(world))
    }
}

pub(crate) struct ResourceCell<'w, T, M: Mode = Read> {
    cell: &'w RefCell<dyn Any>,
    marker: PhantomData<(T, M)>,
}

impl<'w, T: 'static, M: Mode> ResourceCell<'w, T, M> {
    #[inline]
    pub(crate) fn read(&self) -> Ref<'w, T> {
        Ref::map(self.cell.borrow(), |any| {
            any.downcast_ref::<T>()
                .expect("resource has the wrong type")
        })
    }

    #[inline]
    pub(crate) fn write(&self) -> RefMut<'w, T> {
        RefMut::map(self.cell.borrow_mut(), |any| {
            any.downcast_mut::<T>()
                .expect("resource has the wrong type")
        })
    }
}

impl<T: 'static + Default + Hashed, M: Mode> SystemParam for ResourceCell<'_, T, M> {
    type Item<'w> = ResourceCell<'w, T, M>;

    fn init(world: &mut World) {
        world.init_resource::<T>();
    }

    fn fetch<'w>(world: &'w World) -> ResourceCell<'w, T, M> {
        let i = world
            .resources
            .binary_search_by_key(&T::TYPE_UUID, |resource| resource.0)
            .unwrap_or_else(|_| conflict::<T>());
        ResourceCell {
            cell: &*world.resources[i].1,
            marker: PhantomData,
        }
    }
}

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
        fn write_system(mut res_mut: Mut<Resource<IntResource>>) {
            (*res_mut).0 = 234;
        }

        let mut world = World::new();
        write_system.into_system().run(&mut world);

        let res = world.get_resource::<IntResource>();
        assert_eq!((*res.unwrap()).0, 234);
    }
}
