use crate::{
    hashed::Hashed,
    schedule::{Mode, ParamInit, ParamWorld, Read, SystemParam, WithMode, conflict},
    world::World,
};
use std::{
    any::Any,
    cell::{RefCell, RefMut},
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

/// System param for a resource declared with `#[resource]`.
///
/// `Resource<T>` reads the resource and `Mut<Resource<T>>` writes it; both dereference to `T`.
/// The resource exists in every world that has a system using it, starting from its `Default`
/// value.
///
/// # Panics
///
/// The system panics when it runs if another of its params also borrows `T` and one of them
/// writes it, like `Resource<T>` with `Mut<Resource<T>>`.
///
/// # Examples
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[resource]
/// struct Rules {
///     income: u32,
/// }
///
/// #[resource]
/// struct Bank {
///     gold: u32,
/// }
///
/// #[system]
/// fn pay(rules: Resource<Rules>, mut bank: Mut<Resource<Bank>>) {
///     bank.gold += rules.income;
/// }
///
/// fn main() {
///     let mut world = World::build(|setup| setup.insert_resource(Rules { income: 5 }));
///     world.step();
///     world.step();
///     assert_eq!(world.get_resource::<Bank>().unwrap().gold, 10);
/// }
/// ```
pub struct Resource<'w, T: 'static, M: Mode = Read>(pub(crate) M::Guard<'w, T>);

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

impl<T: 'static + Hashed + Default, M: Mode> SystemParam for Resource<'_, T, M> {
    type Item<'w> = Resource<'w, T, M>;

    fn init(world: ParamInit<'_>) {
        world.0.init_resource::<T>();
    }

    fn fetch<'w>(world: ParamWorld<'w>) -> Resource<'w, T, M> {
        Resource(M::fetch(world))
    }
}

#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a local resource",
    note = "declare it with #[resource(local)]"
)]
/// A type declared with `#[resource(local)]`.
///
/// Implemented by the macro; game code doesn't implement it by hand.
pub trait LocalResourceType: 'static + Default {
    /// The hook set with `on_load = method`, run after every load.
    const ON_LOAD: Option<fn(&mut Self, &World)> = None;
}

/// System param for a local resource declared with `#[resource(local)]`.
///
/// `LocalResource<T>` reads it and `Mut<LocalResource<T>>` writes it, with the same borrow rules as
/// [`Resource`]. Local resources aren't saved or hashed, so they may only hold caches that can be
/// rebuilt from saved state: game logic that depends on one gives different results after a load
/// or in a replay.
pub struct LocalResource<'w, T: 'static, M: Mode = Read>(pub(crate) M::Guard<'w, T>);

impl<'w, T: 'static, M0: Mode, M: Mode> WithMode<M> for LocalResource<'w, T, M0> {
    type Out = LocalResource<'w, T, M>;
}

impl<'w, T: 'static, M: Mode> Deref for LocalResource<'w, T, M> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<'w, T: 'static, M: Mode> DerefMut for LocalResource<'w, T, M>
where
    M::Guard<'w, T>: DerefMut,
{
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: LocalResourceType, M: Mode> SystemParam for LocalResource<'_, T, M> {
    type Item<'w> = LocalResource<'w, T, M>;

    fn init(world: ParamInit<'_>) {
        world.0.init_local_resource::<T>();
    }

    fn fetch<'w>(world: ParamWorld<'w>) -> LocalResource<'w, T, M> {
        LocalResource(M::fetch_local(world))
    }
}

pub(crate) struct ResourceCell<'w, T, M: Mode = Read> {
    cell: &'w RefCell<T>,
    marker: PhantomData<M>,
}

impl<'w, T: 'static, M: Mode> ResourceCell<'w, T, M> {
    #[inline]
    pub(crate) fn read(&self) -> &T {
        // SAFETY: skips the `RefCell` borrow. Sound only while no `RefMut` of this resource is
        // live, so callers must never keep the reference past one expression. Enforced by
        // visibility: `read` is `pub(crate)` and only used on `EntityGenerations` (`pub(crate)`),
        // in single expressions in `entity.rs`; its only writer, `despawn`, holds `write()` for
        // one statement.
        unsafe { &*self.cell.as_ptr() }
    }

    #[inline]
    pub(crate) fn write(&self) -> RefMut<'w, T> {
        self.cell.borrow_mut()
    }
}

impl<T: 'static + Default + Hashed, M: Mode> SystemParam for ResourceCell<'_, T, M> {
    type Item<'w> = ResourceCell<'w, T, M>;

    fn init(world: ParamInit<'_>) {
        world.0.init_resource::<T>();
    }

    fn fetch<'w>(world: ParamWorld<'w>) -> ResourceCell<'w, T, M> {
        let world = world.0;
        let i = world
            .resources
            .binary_search_by_key(&T::TYPE_UUID, |resource| resource.0)
            .unwrap_or_else(|_| conflict::<T>());
        let cell: &RefCell<dyn Any> = &*world.resources[i].1;
        assert!(
            cell.try_borrow().map_or(true, |any| any.is::<T>()),
            "resource has the wrong type"
        );
        ResourceCell {
            // SAFETY: the cell was found by `T::TYPE_UUID`, and registration panics on a UUID
            // collision, so it holds a `T`. The `is::<T>` check above confirms it whenever the
            // cell isn't mutably borrowed.
            cell: unsafe { &*(cell as *const RefCell<dyn Any> as *const RefCell<T>) },
            marker: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use std::rc::Rc;

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
        assert_eq!(res.unwrap().0, 234);
    }

    #[resource(local)]
    struct LocalInt(pub u64);

    #[resource(local)]
    struct NotSerializable(pub Rc<u64>);

    #[test]
    fn world_initializes_local_resource_through_system() {
        #[resource(local)]
        struct ParamOnlyLocal;

        #[system]
        fn param_only_local_system(_local: LocalResource<ParamOnlyLocal>) {}

        let world = World::new();

        assert!(world.has_local_resource::<ParamOnlyLocal>());
    }

    #[test]
    fn system_reads_local_resource() {
        #[system]
        fn read_local_system(local: LocalResource<LocalInt>) {
            assert_eq!((*local).0, 94);
        }

        let mut world = World::new();
        world.insert_local_resource(LocalInt(94));

        read_local_system.into_system().run(&mut world);
    }

    #[test]
    fn system_writes_local_resource() {
        #[system]
        fn write_local_system(mut local: Mut<LocalResource<LocalInt>>) {
            (*local).0 = 234;
        }

        let mut world = World::new();
        world.init_local_resource::<LocalInt>();

        write_local_system.into_system().run(&mut world);

        assert_eq!(world.get_local_resource::<LocalInt>().unwrap().0, 234);
    }

    #[test]
    fn local_resource_works_inside_custom_param() {
        #[param]
        struct LocalView {
            local: LocalResource<LocalInt>,
            #[read]
            other: LocalResource<NotSerializable>,
        }

        #[system]
        fn local_view_system(mut view: Mut<LocalView>) {
            (*view.local).0 = *(*view.other).0;
        }

        let mut world = World::new();
        world.init_local_resource::<LocalInt>();
        world.insert_local_resource(NotSerializable(Rc::new(7)));

        local_view_system.into_system().run(&mut world);

        assert_eq!(world.get_local_resource::<LocalInt>().unwrap().0, 7);
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn local_resource_read_and_write_conflict_panics() {
        #[system]
        fn conflicting_local_system(
            _read: LocalResource<LocalInt>,
            _write: Mut<LocalResource<LocalInt>>,
        ) {
        }

        let mut world = World::new();
        world.init_local_resource::<LocalInt>();

        conflicting_local_system.into_system().run(&mut world);
    }

    #[test]
    fn world_access_sees_local_resources() {
        #[system]
        fn world_access_local_system(world: WorldAccess, _write: Mut<LocalResource<LocalInt>>) {
            assert!(world.get_local_resource::<LocalInt>().is_none());
            assert_eq!(*world.get_local_resource::<NotSerializable>().unwrap().0, 3);
        }

        let mut world = World::new();
        world.init_local_resource::<LocalInt>();
        world.insert_local_resource(NotSerializable(Rc::new(3)));

        world_access_local_system.into_system().run(&mut world);
    }

    #[test]
    fn init_local_resource_keeps_existing_value() {
        let mut world = World::new();
        world.insert_local_resource(LocalInt(7));

        world.init_local_resource::<LocalInt>();

        assert_eq!(world.get_local_resource::<LocalInt>().unwrap().0, 7);
    }

    #[test]
    fn insert_local_resource_replaces_value() {
        let mut world = World::new();
        world.insert_local_resource(LocalInt(1));

        world.insert_local_resource(LocalInt(2));

        assert_eq!(world.get_local_resource::<LocalInt>().unwrap().0, 2);
    }

    #[test]
    fn remove_local_resource_clears_presence() {
        let mut world = World::new();
        world.init_local_resource::<LocalInt>();

        assert!(world.remove_local_resource::<LocalInt>());

        assert!(!world.has_local_resource::<LocalInt>());
        assert!(!world.remove_local_resource::<LocalInt>());
    }

    #[test]
    fn local_resources_stay_reachable_after_removal() {
        let mut world = World::new();
        world.insert_local_resource(LocalInt(1));
        world.insert_local_resource(NotSerializable(Rc::new(2)));

        world.remove_local_resource::<LocalInt>();

        assert_eq!(*world.get_local_resource::<NotSerializable>().unwrap().0, 2);
    }

    #[test]
    fn get_local_resource_returns_none_while_mutably_borrowed() {
        let mut world = World::new();
        world.init_local_resource::<LocalInt>();

        let _guard = world.get_local_resource_mut::<LocalInt>().unwrap();

        assert!(world.get_local_resource::<LocalInt>().is_none());
    }

    #[test]
    fn get_local_resource_mut_returns_none_while_borrowed() {
        let mut world = World::new();
        world.init_local_resource::<LocalInt>();

        let _guard = world.get_local_resource::<LocalInt>().unwrap();

        assert!(world.get_local_resource_mut::<LocalInt>().is_none());
        assert!(world.get_local_resource::<LocalInt>().is_some());
    }
}
