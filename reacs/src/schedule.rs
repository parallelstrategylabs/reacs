use crate::world::World;
use std::any::TypeId;

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
    fn access() -> Access;
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
                check_conflicts(&[$($param::access()),*]);

                SystemEntry {
                    init_fn: Box::new(move |world: &mut World| {
                        $(
                            $param::init(world);
                        )*
                    }),

                    run_fn: Box::new(move |world: &mut World| {
                        fn call_inner<$($param),*>(
                            mut f: impl FnMut($($param),*),
                            $($param: $param),*
                        ) {
                            f($($param),*)
                        }

                        $(
                            let $param = unsafe { $param::fetch(world) };
                        )*
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

#[derive(Clone, Copy)]
pub struct Access {
    pub type_id: TypeId,
    pub type_name: &'static str,
    pub mutable: bool,
}

fn check_conflicts(accesses: &[Access]) {
    for i in 0..accesses.len() {
        for j in (i + 1)..accesses.len() {
            let (a, b) = (accesses[i], accesses[j]);
            if a.type_id == b.type_id && (a.mutable || b.mutable) {
                panic!(
                    "Resource `{}` conflicts during access (params {} & {})",
                    a.type_name,
                    i + 1,
                    j + 1
                );
            }
        }
    }
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

    #[system]
    fn auto_registered_system() {}

    #[test]
    #[should_panic]
    fn system_panics_with_conflict() {
        fn conflicting_system(_res: Resource<IntResource>, _res_mut: ResourceMut<IntResource>) {}

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
            _res_mut_bool: ResourceMut<BoolResource>,
            _res_int: Resource<IntResource>,
        ) {
        }

        #[system]
        fn three_param_system(
            _res_custom: ResourceMut<CustomResource>,
            _res_int: Resource<IntResource>,
            _res_bool: Resource<BoolResource>,
        ) {
        }

        let mut world = World::new();

        two_param_system.into_system().run(&mut world);
        three_param_system.into_system().run(&mut world);
    }
}
