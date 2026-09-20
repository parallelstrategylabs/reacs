use crate::world::World;
use std::any::TypeId;

use std::ptr::addr_of_mut;

static mut SYSTEM_REGISTRY: Option<Vec<StoredSystem>> = None;

pub fn register_system(system: StoredSystem) {
    unsafe {
        let ptr = addr_of_mut!(SYSTEM_REGISTRY);
        (*ptr).get_or_insert_with(Vec::new).push(system);
    }
}

pub fn get_registered_systems() -> &'static Vec<StoredSystem> {
    unsafe {
        let ptr = addr_of_mut!(SYSTEM_REGISTRY);
        (*ptr).get_or_insert_with(Vec::new)
    }
}

pub struct StoredSystem {
    pub run_fn: Box<dyn FnMut(&World)>,
}

impl StoredSystem {
    pub fn run(&mut self, world: &World) {
        (self.run_fn)(&world)
    }
}

pub trait SystemParam {
    type Item<'w>;
    fn fetch<'w>(world: &'w World) -> Self::Item<'w>;
    fn access() -> Access;
}

pub trait IntoSystem<P> {
    fn into_system(self) -> StoredSystem;
}

macro_rules! impl_into_system_tuples {
    ($($param:ident),*) => {
        impl<F, $($param),*> IntoSystem<($($param,)*)> for F
        where
            F: FnMut($($param),*) + 'static,
            F: for<'w> FnMut($($param::Item<'w>),*),
            $($param: SystemParam + 'static),*
        {
            #[allow(nonstandard_style)]
            fn into_system(mut self) -> StoredSystem {
                check_conflicts(&[$($param::access()),*]);
                StoredSystem {
                    run_fn: Box::new(move |world| {
                        $(
                            let $param = $param::fetch(world);
                        )*
                        (self)($($param),*);
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

#[cfg(test)]

mod tests {
    use crate::{prelude::*, schedule::get_registered_systems};

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
    fn system_auto_registers_in_scheduler() {
        assert!(
            get_registered_systems().len() > 0,
            "No system was registered!"
        );
    }

    #[test]
    #[should_panic]
    fn system_panics_with_conflict() {
        fn conflicting_system(_res: Resource<IntResource>, _res_mut: ResourceMut<IntResource>) {}
        let world = World::new();
        conflicting_system.into_system().run(&world);
    }

    #[test]
    fn system_runs_with_no_params() {
        fn system() {}

        let world = World::new();
        system.into_system().run(&world);
    }

    #[test]
    fn system_runs_with_many_params() {
        fn two_param_system(
            _res_mut_bool: ResourceMut<BoolResource>,
            _res_int: Resource<IntResource>,
        ) {
        }

        fn three_param_system(
            _res_custom: ResourceMut<CustomResource>,
            _res_int: Resource<IntResource>,
            _res_bool: Resource<BoolResource>,
        ) {
        }

        let mut world = World::new();
        world.init_resource::<BoolResource>();
        world.init_resource::<IntResource>();
        world.init_resource::<CustomResource>();

        two_param_system.into_system().run(&world);
        three_param_system.into_system().run(&world);
    }
}
