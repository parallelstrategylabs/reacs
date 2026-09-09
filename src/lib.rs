use core::{
    any::{Any, TypeId},
    cell::UnsafeCell,
};

use std::{cell::RefCell, collections::HashMap};

use crate::{
    schedule::{Resource, ResourceMut, Schedule},
    world::World,
};

mod schedule;
mod world;

pub fn test_system1(param1: Resource<Movimento>, param2: Resource<Movimento>) {}
pub fn test_system2(param2: ResourceMut<Movimento>) {}

pub struct Movimento {
    pub velocidade: f32,
}

pub fn test_system3(mut param1: ResourceMut<Movimento>) {
    param1.velocidade += 1.0;
}

pub fn dangerous_system(mov: Resource<Movimento>, mut mov_mut: ResourceMut<Movimento>) {
    mov_mut.velocidade += 1.0;
}

pub fn first_test() {
    let mut world = World::new();
    let mut scheduler = Schedule::new();

    world.storage.insert(
        TypeId::of::<Movimento>(),
        Box::new(RefCell::new(Movimento { velocidade: 5.0 })),
    );

    scheduler.add_system(test_system1);
    scheduler.add_system(test_system2);
    scheduler.add_system(test_system3);
    // scheduler.add_system(dangerous_system);

    for system in scheduler.systems.iter_mut() {
        (system.run_fn)(&world)
    }
}

pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test() {
        first_test();
        assert!(true);
    }

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
