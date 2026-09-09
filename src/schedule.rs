use crate::world::World;
use std::{
    any::{Any, TypeId},
    cell::{Ref, RefCell, RefMut},
    ops::{Deref, DerefMut},
};

pub struct Schedule {
    pub systems: Vec<StoredSystem>,
}

impl Schedule {
    pub fn new() -> Self {
        Self {
            systems: Vec::new(),
        }
    }

    pub fn add_system<P, F: IntoSystem<P>>(&mut self, system: F) {
        self.systems.push(system.into_system());
    }
}

pub struct StoredSystem {
    pub run_fn: Box<dyn FnMut(&World)>,
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

impl<T: 'static> SystemParam for Resource<'_, T> {
    type Item<'w> = Resource<'w, T>;

    fn fetch<'w>(world: &'w World) -> Self::Item<'w> {
        let box_cell = world
            .storage
            .get(&TypeId::of::<T>())
            .expect("Resource not initialized");

        let borrow: Ref<'w, dyn Any> = box_cell.borrow();
        let resource_ref = Ref::map(borrow, |any| {
            any.downcast_ref::<T>().expect("Type does not match TypeId")
        });

        Resource(resource_ref)
    }

    fn access() -> Access {
        Access {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            mutable: false,
        }
    }
}

impl<T: 'static> SystemParam for ResourceMut<'_, T> {
    type Item<'w> = ResourceMut<'w, T>;

    fn fetch<'w>(world: &'w World) -> Self::Item<'w> {
        let box_cell = world
            .storage
            .get(&TypeId::of::<T>())
            .expect("Not initialized");

        let borrow: RefMut<'w, dyn Any> = box_cell.borrow_mut();
        let resource_ref = RefMut::map(borrow, |any| {
            any.downcast_mut::<T>().expect("Type does not match TypeId")
        });

        ResourceMut(resource_ref)
    }

    fn access() -> Access {
        Access {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            mutable: true,
        }
    }
}
