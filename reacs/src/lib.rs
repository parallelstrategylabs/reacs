extern crate self as reacs;

pub mod bit_set;
pub mod component;
pub mod entity;
pub mod free_list;
pub mod hashed;
pub mod paged_vec;
pub mod registry;
pub mod resource;
pub mod schedule;
pub mod world;

pub mod prelude {
    pub use crate::hashed::Hashed;
    pub use crate::resource::{Resource, ResourceMut};
    pub use crate::schedule::{IntoSystem, SystemParam};
    pub use crate::world::World;

    pub use reacs_derive::{Hashed as DeriveHashed, resource, system};
}

#[cfg(test)]
mod tests {
    use crate::{prelude::*, registry::get_registry_entry, schedule::get_registered_systems};

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
    fn resource_auto_registers_in_registry() {
        let entry = get_registry_entry(IntResource::TYPE_UUID);
        assert!(entry.is_some(), "Resource registration failed!");
    }

    #[test]
    fn resource_serialize_deserialize_roundtrip() {
        let entry = get_registry_entry(IntResource::TYPE_UUID).unwrap();
        let original = IntResource(42);

        let bytes = (entry.serialize)(&original as &dyn core::any::Any);
        let restored = (entry.deserialize)(&bytes);

        let restored = restored
            .downcast::<IntResource>()
            .expect("Downcast failed, incorrect type!");

        assert_eq!(restored.0, original.0, "Resource deserialization failed!");
    }

    #[test]
    fn world_inserts_resource() {
        let mut world = World::new();
        world.init_resource::<IntResource>();

        assert!(
            world.has_resource::<IntResource>(),
            "Resource insertion failed!"
        );
    }

    #[test]
    fn world_gets_resource() {
        let mut world = World::new();
        world.insert_resource(IntResource(78));

        let value = world
            .get_resource::<IntResource>()
            .expect("Resource not initialized!");

        assert_eq!((*value).0, 78);
    }

    #[test]
    fn world_removes_resource() {
        let mut world = World::new();

        world.init_resource::<IntResource>();
        world.remove_resource::<IntResource>();

        assert!(
            !world.has_resource::<IntResource>(),
            "Resource removal failed!"
        );
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
    fn resource_reads() {
        fn read_system(res: Resource<IntResource>) {
            assert_eq!((*res).0, 94);
        }

        let mut world = World::new();
        world.insert_resource(IntResource(94));

        read_system.into_system().run(&world);
    }

    #[test]
    fn resource_writes() {
        fn write_system(mut res_mut: ResourceMut<IntResource>) {
            (*res_mut).0 = 234;
        }

        let mut world = World::new();
        world.init_resource::<IntResource>();

        write_system.into_system().run(&world);

        let res = world.get_resource::<IntResource>();
        assert_eq!((*res.unwrap()).0, 234);
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
