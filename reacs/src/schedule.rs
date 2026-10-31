use crate::hashed::Hashed;
use crate::world::World;

use std::cell::{Ref, RefMut};
use std::ops::Deref;
use std::ptr::addr_of_mut;

pub static mut SYSTEM_REGISTRY: Option<Vec<SystemEntry>> = None;

pub fn register_system(system: SystemEntry) {
    unsafe {
        let ptr = addr_of_mut!(SYSTEM_REGISTRY);
        (*ptr).get_or_insert_with(Vec::new).push(system);
    }
}

pub fn get_registered_systems() -> &'static mut Vec<SystemEntry> {
    unsafe {
        let ptr = addr_of_mut!(SYSTEM_REGISTRY);
        (*ptr).get_or_insert_with(Vec::new)
    }
}

pub struct SystemEntry {
    pub init_fn: Box<dyn FnMut(&mut World)>,
    pub run_fn: Box<dyn FnMut(&mut World)>,
}

impl SystemEntry {
    pub fn init(&mut self, world: &mut World) {
        (self.init_fn)(world);
    }

    pub fn run(&mut self, world: &mut World) {
        (self.run_fn)(world);
    }
}

pub trait SystemParam {
    type Item<'w>;

    fn init(world: &mut World);
    fn fetch<'w>(world: &'w World) -> Self::Item<'w>;
}

pub trait IntoSystem<P> {
    fn into_system(self) -> SystemEntry;
}

macro_rules! impl_into_system_tuples {
    ($($param:ident),*) => {
        impl<F, $($param),*> IntoSystem<($($param,)*)> for F
        where
            F: FnMut($($param),*) + 'static,
            F: for<'w> FnMut($($param::Item<'w>),*),
            $($param: SystemParam),*
        {
            #[allow(nonstandard_style, unused_variables, unused_mut)]
            fn into_system(mut self) -> SystemEntry {
                SystemEntry {
                    init_fn: Box::new(move |world: &mut World| {
                        $( $param::init(world); )*
                    }),

                    run_fn: Box::new(move |world: &mut World| {
                        fn call_inner<$($param),*>(
                            mut f: impl FnMut($($param),*),
                            $($param: $param),*
                        ) {
                            f($($param),*)
                        }

                        $( let $param = $param::fetch(world); )*
                        call_inner(&mut self, $($param),*);
                    }),
                }
            }
        }
    };
}

impl_into_system_tuples!();
impl_into_system_tuples!(P1);
impl_into_system_tuples!(P1, P2);
impl_into_system_tuples!(P1, P2, P3);
impl_into_system_tuples!(P1, P2, P3, P4);
impl_into_system_tuples!(P1, P2, P3, P4, P5);
impl_into_system_tuples!(P1, P2, P3, P4, P5, P6);
impl_into_system_tuples!(P1, P2, P3, P4, P5, P6, P7);
impl_into_system_tuples!(P1, P2, P3, P4, P5, P6, P7, P8);

pub struct Read;
pub struct Write;

pub trait Mode {
    type Guard<'w, T: 'w>: Deref<Target = T>;
    fn fetch<'w, T: 'static + Hashed>(world: &'w World) -> Self::Guard<'w, T>;
}

impl Mode for Read {
    type Guard<'w, T: 'w> = Ref<'w, T>;
    fn fetch<'w, T: 'static + Hashed>(world: &'w World) -> Ref<'w, T> {
        world.get_resource::<T>().unwrap_or_else(|| conflict::<T>())
    }
}

impl Mode for Write {
    type Guard<'w, T: 'w> = RefMut<'w, T>;
    fn fetch<'w, T: 'static + Hashed>(world: &'w World) -> RefMut<'w, T> {
        world
            .get_resource_mut_silent::<T>()
            .unwrap_or_else(|| conflict::<T>())
    }
}

pub trait WithMode<M: Mode> {
    type Out;
}

pub type Mut<P> = <P as WithMode<Write>>::Out;

#[cold]
fn conflict<T>() -> ! {
    panic!(
        "Resource `{}` is missing or already borrowed (conflicting params?)",
        std::any::type_name::<T>()
    )
}

impl World {
    #[inline]
    pub fn init_param<P: SystemParam>(&mut self) {
        P::init(self);
    }

    #[inline]
    pub fn fetch_param<P: SystemParam>(&self) -> P::Item<'_> {
        P::fetch(self)
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[resource]
    struct IntResource(pub u64);

    #[resource]
    struct BoolResource(pub bool);

    #[resource]
    struct CustomResource {
        pub value: i32,
        pub active: bool,
    }

    #[test]
    fn world_initializes_param_through_system() {
        #[system]
        fn system(_res: Resource<IntResource>) {}

        let world = World::new();
        assert!(world.has_resource::<IntResource>());
    }

    #[test]
    #[should_panic]
    fn system_panics_with_conflict() {
        #[system]
        fn conflicting_system(_res: Resource<IntResource>, _res_mut: Mut<Resource<IntResource>>) {}

        let mut world = World::new();
        conflicting_system.into_system().run(&mut world);
    }

    #[test]
    fn system_runs_with_no_params() {
        #[system]
        fn system() {}

        let mut world = World::new();
        system.into_system().run(&mut world);
    }

    #[test]
    fn system_runs_with_many_params() {
        #[system]
        fn two_param_system(
            _res_mut_bool: Mut<Resource<BoolResource>>,
            _res_int: Resource<IntResource>,
        ) {
        }

        #[system]
        fn three_param_system(
            _res_custom: Mut<Resource<CustomResource>>,
            _res_int: Resource<IntResource>,
            _res_bool: Resource<BoolResource>,
        ) {
        }

        let mut world = World::new();

        two_param_system.into_system().run(&mut world);
        three_param_system.into_system().run(&mut world);
    }
}
