use crate::world::World;
use std::{
    any::{Any, TypeId},
    cell::{Ref, RefCell, RefMut},
    ops::{Deref, DerefMut},
};

use std::ptr::addr_of_mut;

static mut SYSTEM_REGISTRY: Option<Vec<StoredSystem>> = None;

pub fn register_system(system: StoredSystem) {
    unsafe {
        let ptr = addr_of_mut!(SYSTEM_REGISTRY);
        (*ptr).get_or_insert_with(Vec::new).push(system);
    }
}

pub fn get_registered_systems() -> Vec<StoredSystem> {
    unsafe {
        let ptr = addr_of_mut!(SYSTEM_REGISTRY);
        (*ptr).take().unwrap_or_default()
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